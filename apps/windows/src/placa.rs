//! A placa de espera: o que a câmera virtual mostra quando não há vídeo.
//!
//! # Por que uma placa, e não preto
//!
//! Do lado do app consumidor — Zoom, Meet, OBS — **"sem imagem" e "travou" são indistinguíveis**.
//! A pessoa abre o Meet, escolhe a câmera Quall, vê preto, e não tem como saber se falta abrir
//! algo no celular, se a rede caiu ou se o Quall morreu. A placa responde essa pergunta no único
//! lugar onde a pessoa está olhando: dentro da imagem da câmera.
//!
//! São **duas** placas, em dois processos, e a diferença importa:
//!
//! | Quem desenha | Quando aparece | O que diz |
//! |---|---|---|
//! | a fonte de mídia (`fonte/src/quadros.rs`) | o Quall **não** está rodando | padrão de bancada |
//! | este arquivo, no host | o Quall está rodando e **não há vídeo** | o que fazer agora |
//!
//! A fonte não pode dizer "abra o Quall" com autoridade nenhuma, porque ela vive dentro do
//! `svchost` e o que ela sabe é só que o cano não está lá. O host sabe mais: sabe se está
//! procurando, conectando, pareado e esperando o primeiro quadro, ou se a sessão caiu. Cada um
//! desses estados é uma frase diferente.
//!
//! # Como o texto é desenhado
//!
//! GDI: um DIB de 32 bits, `DrawTextW` com Segoe UI, e uma conversão BGRA→NV12 em CPU **uma vez
//! por mensagem** — não por quadro. A placa é estática; o que muda a cada quadro é uma barrinha
//! que anda, escrita direto no plano Y do buffer já pronto. Custa dezenas de microssegundos, não
//! uma conversão de imagem.
//!
//! A barrinha não é enfeite: uma placa parada não distingue "o Quall está esperando você" de "o
//! Quall travou" — que é o mesmo problema que a placa existe para resolver, um nível abaixo.

use anyhow::{Context, Result};

use windows::core::PCWSTR;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::*;

use crate::cano;
use crate::idioma::t;

/// O que a placa diz. Um estado por frase — e a frase muda porque o que a pessoa precisa fazer
/// muda.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Estado {
    Procurando,
    Conectando,
    /// Sessão de pé, nenhum quadro ainda. Quase sempre dura milissegundos; quando dura, é porque
    /// o IDR não veio, e aí a frase certa é essa mesma.
    EsperandoImagem,
    SemAparelho,
    Caiu,
}

impl Estado {
    /// No idioma de agora: a placa é desenhada de novo quando o idioma muda ([`Placa::serve_para`]).
    fn titulo(self) -> &'static str {
        match self {
            Estado::Procurando => t("Procurando aparelhos…"),
            Estado::Conectando => t("Conectando…"),
            Estado::EsperandoImagem => t("Conectado — esperando a imagem"),
            Estado::SemAparelho => t("Nenhum aparelho conectado"),
            Estado::Caiu => t("O aparelho saiu"),
        }
    }

    fn detalhe(self) -> &'static str {
        match self {
            Estado::Procurando => t("Quall está procurando na rede local."),
            Estado::Conectando => t("Pareando com o aparelho escolhido."),
            Estado::EsperandoImagem => t("O primeiro quadro chega assim que o emissor mandar uma imagem completa."),
            Estado::SemAparelho => t("No celular: abra o Quall, escolha o que transmitir e toque em Espelhar."),
            Estado::Caiu => t("Para voltar, toque em Espelhar de novo no aparelho."),
        }
    }
}

/// Uma placa já convertida para NV12 1920x1080, pronta para ir ao cano.
pub struct Placa {
    pub estado: Estado,
    /// A versão do idioma com que o texto foi desenhado (`crate::idioma::versao`).
    versao_do_idioma: u32,
    nv12: Vec<u8>,
}

impl Placa {
    pub fn nova(estado: Estado) -> Result<Placa> {
        let versao_do_idioma = crate::idioma::versao();
        let bgra = desenhar(estado.titulo(), estado.detalhe())?;
        Ok(Placa {
            estado,
            versao_do_idioma,
            nv12: para_nv12(&bgra),
        })
    }

    /// A placa já desenhada serve para `estado`? Não quando o estado mudou, nem quando o idioma
    /// mudou depois do desenho (a troca PT | EN vale na hora, também dentro da câmera virtual).
    pub fn serve_para(&self, estado: Estado) -> bool {
        self.estado == estado && self.versao_do_idioma == crate::idioma::versao()
    }

    /// Devolve o quadro da placa com a barrinha na posição `tique`.
    pub fn quadro(&self, tique: u64) -> Vec<u8> {
        let mut buf = self.nv12.clone();
        let w = cano::LARGURA as usize;
        let h = cano::ALTURA as usize;
        // Barra de 120 px que atravessa a tela numa faixa de 6 px perto do rodapé.
        let x0 = ((tique * 12) % (w as u64 + 120)).saturating_sub(120) as usize;
        let x1 = (x0 + 120).min(w);
        for y in (h - 80)..(h - 74) {
            let base = y * w;
            for x in x0..x1 {
                buf[base + x] = 200;
            }
        }
        buf
    }
}

/// Desenha o texto num DIB BGRA de 1920x1080 e devolve os bytes.
fn desenhar(titulo: &str, detalhe: &str) -> Result<Vec<u8>> {
    let largura = cano::LARGURA as i32;
    let altura = cano::ALTURA as i32;

    unsafe {
        let dc_tela = GetDC(None);
        let dc = CreateCompatibleDC(Some(dc_tela));
        if dc.is_invalid() {
            let _ = ReleaseDC(None, dc_tela);
            anyhow::bail!("CreateCompatibleDC falhou"); // i18n: fora (diário)
        }

        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: largura,
                // Negativo: de cima para baixo, para a linha 0 do buffer ser a linha 0 da imagem.
                biHeight: -altura,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .context("CreateDIBSection")?; // i18n: fora (diário)
        let antigo = SelectObject(dc, HGDIOBJ(bitmap.0));

        // Fundo: um gradiente escuro, escrito direto no buffer. Mais barato e mais previsível que
        // pedir ao GDI um `GradientFill`, e não depende de msimg32.
        let total = (largura * altura) as usize;
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u8, total * 4);
        for y in 0..altura as usize {
            let tom = 18 + (y * 26 / altura as usize) as u8;
            for x in 0..largura as usize {
                let p = (y * largura as usize + x) * 4;
                pixels[p] = tom + 8; // B
                pixels[p + 1] = tom; // G
                pixels[p + 2] = tom; // R
                pixels[p + 3] = 255;
            }
        }

        SetBkMode(dc, TRANSPARENT);

        // Marca: uma barra vertical clara à esquerda do texto, para a placa ser reconhecível como
        // "Quall" de relance, sem depender de a fonte ter carregado.
        for y in 250..420usize {
            for x in 120..128usize {
                let p = (y * largura as usize + x) * 4;
                pixels[p] = 235;
                pixels[p + 1] = 235;
                pixels[p + 2] = 235;
            }
        }

        escrever(dc, "Quall", 160, 240, 300, 44, 700, 0x00B0_B0B0);
        escrever(dc, titulo, 160, 300, largura - 220, 54, 600, 0x00FF_FFFF);
        escrever(dc, detalhe, 160, 380, largura - 220, 34, 400, 0x00C8_C8C8);

        // Sem isto o `to_vec` abaixo lê o DIB **antes** de o GDI ter escrito nele: as chamadas de
        // desenho são enfileiradas em lote, e o texto sairia faltando de forma intermitente — o
        // pior tipo de defeito, porque às vezes funciona.
        let _ = GdiFlush();
        let saida = pixels.to_vec();

        SelectObject(dc, antigo);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(dc);
        let _ = ReleaseDC(None, dc_tela);
        Ok(saida)
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn escrever(
    dc: HDC,
    texto: &str,
    x: i32,
    y: i32,
    largura: i32,
    altura_fonte: i32,
    peso: i32,
    cor: u32,
) {
    let nome: Vec<u16> = "Segoe UI\0".encode_utf16().collect();
    let fonte = CreateFontW(
        -altura_fonte,
        0,
        0,
        0,
        peso,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        // `DEFAULT_PITCH | FF_DONTCARE` são os dois zero; escrever `0` evita depender de como
        // esta versão do `windows-rs` tipa as duas constantes.
        0,
        PCWSTR(nome.as_ptr()),
    );
    let antiga = SelectObject(dc, HGDIOBJ(fonte.0));
    SetTextColor(dc, windows::Win32::Foundation::COLORREF(cor));
    let mut r = RECT {
        left: x,
        top: y,
        right: x + largura,
        bottom: y + altura_fonte * 3,
    };
    let mut u16s: Vec<u16> = texto.encode_utf16().collect();
    let _ = DrawTextW(
        dc,
        &mut u16s,
        &mut r,
        DT_LEFT | DT_TOP | DT_WORDBREAK | DT_NOPREFIX,
    );
    SelectObject(dc, antiga);
    let _ = DeleteObject(HGDIOBJ(fonte.0));
}

/// BGRA → NV12 em faixa **limitada** (BT.601), que é o que a câmera anuncia
/// (`MF_MT_VIDEO_NOMINAL_RANGE = MFNominalRange_16_235`). Converter para faixa completa aqui e
/// anunciar limitada é exatamente o defeito que `docs/contrato-sidecar.md` existe para evitar.
fn para_nv12(bgra: &[u8]) -> Vec<u8> {
    let w = cano::LARGURA as usize;
    let h = cano::ALTURA as usize;
    let mut saida = vec![0u8; cano::BYTES_NV12];

    for y in 0..h {
        for x in 0..w {
            let p = (y * w + x) * 4;
            let b = bgra[p] as f32;
            let g = bgra[p + 1] as f32;
            let r = bgra[p + 2] as f32;
            let luma = 0.299 * r + 0.587 * g + 0.114 * b;
            saida[y * w + x] = (16.0 + 219.0 * luma / 255.0).round().clamp(16.0, 235.0) as u8;
        }
    }
    // Croma por sub-amostragem 2x2, com a média dos quatro pixels — a placa é quase cinza, então
    // isto fica perto de 128, mas fazer a conta certa evita um tom esverdeado se um dia a placa
    // ganhar cor.
    let inicio_uv = w * h;
    for cy in 0..h / 2 {
        for cx in 0..w / 2 {
            let mut somab = 0f32;
            let mut somag = 0f32;
            let mut somar = 0f32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let p = ((cy * 2 + dy) * w + cx * 2 + dx) * 4;
                    somab += bgra[p] as f32;
                    somag += bgra[p + 1] as f32;
                    somar += bgra[p + 2] as f32;
                }
            }
            let (b, g, r) = (somab / 4.0, somag / 4.0, somar / 4.0);
            let luma = 0.299 * r + 0.587 * g + 0.114 * b;
            let u = 128.0 + 112.0 * (b - luma) / (255.0 * 0.886);
            let v = 128.0 + 112.0 * (r - luma) / (255.0 * 0.701);
            let i = inicio_uv + cy * w + cx * 2;
            saida[i] = u.round().clamp(16.0, 240.0) as u8;
            saida[i + 1] = v.round().clamp(16.0, 240.0) as u8;
        }
    }
    saida
}
