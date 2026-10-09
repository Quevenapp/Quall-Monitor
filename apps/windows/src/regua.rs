//! A régua de blocos, lida de volta do quadro **decodificado** — um contador, não uma imagem.
//!
//! # Por que ela existe, e por que ela é a única prova de pixel que esta frente pode dar
//!
//! "O app do Windows exibiu vídeo" é uma afirmação sobre pixels, e contador de quadro não a
//! sustenta: um decodificador pode entregar mil texturas cinzas com todos os contadores fechando.
//! A saída óbvia — fotografar a janela — esbarra na regra da casa: **um vídeo de bancada pode
//! conter a vida do usuário**, e uma frente já abriu um quadro de `.h264` com o WhatsApp Web do
//! Dell à mostra.
//!
//! A régua resolve os dois. O gerador de fonte escreve o número do quadro em quatro blocos
//! chapados de 64×64 no canto superior esquerdo; este módulo lê a média do **miolo** de cada bloco
//! e reconstrói o número. Se ele bate com um quadro plausível, os pixels que saíram do
//! decodificador são os pixels que entraram no encoder — e o que fica registrado é **um inteiro**,
//! nunca um pixel.
//!
//! # A condição que torna isto legítimo, e ela não é negociável
//!
//! A régua só existe na **origem sintética nossa** (`apps/ios/Receptor/Ferramentas/gerar-fonte.swift`).
//! Numa origem que seja a tela de alguém ela simplesmente não está lá e [`ler`] devolve `None` —
//! o que é o comportamento certo e não é erro. Mas isso é sorte, não desenho: por isso o leitor
//! fica atrás de um sinalizador de bancada (`--regua`), **não** lê nada fora dos quatro blocos, e
//! **não** tem caminho para gravar imagem. O que ele produz são inteiros e contagens.
//!
//! # A cópia das constantes, e por que ela é assumida
//!
//! Os valores abaixo são idênticos aos de `apps/ios/Receptor/Comum/Marca.swift` e
//! `apps/ios/Receptor/Ferramentas/gerar-fonte.swift` — que já são uma cópia um do outro, por
//! alvos de compilação diferentes. Esta é a terceira, num terceiro idioma. O risco é real e o
//! sintoma é reconhecível: "decodifica mas a marca nunca bate", que aponta para o decodificador
//! quando o defeito é a constante. Por isso [`ilegiveis`] é contado separado de "não tem régua":
//! um quadro **sem** régua e um quadro **com** régua ilegível são diagnósticos opostos.

/// Quantos blocos, e portanto quantos dígitos na base 4.
pub const DIGITOS: usize = 4;
/// `4^DIGITOS`. A 30 fps a régua dá a volta em 8,5 s.
pub const MODULO: u32 = 256;
/// Lado de cada bloco, em pixels. Múltiplo do macrobloco de 16 do H.264, de propósito.
pub const LADO: usize = 64;

/// A luma que representa o dígito `d`: 40, 90, 140, 190. Espaçados de 50 para a quantização não
/// confundir dois deles.
pub fn luma_do_digito(d: u32) -> u8 {
    (40 + d * 50) as u8
}

/// O dígito mais próximo desta luma, ou `None` se nenhum estiver a menos de 21 níveis.
///
/// **Nada é "corrigido"**: um bloco fora da tolerância aborta a leitura do quadro inteiro. Um
/// leitor que arredonda para o dígito mais próximo custe o que custar sempre devolve um número, e
/// um número que sempre existe não prova nada.
pub fn digito(media: i32) -> Option<u32> {
    let mut melhor: Option<u32> = None;
    let mut erro = 21;
    for d in 0..4u32 {
        let e = (media - luma_do_digito(d) as i32).abs();
        if e < erro {
            erro = e;
            melhor = Some(d);
        }
    }
    melhor
}

/// Lê a régua do plano de luma de um quadro decodificado.
///
/// `luma` é o plano 0 de um NV12 (as `altura` primeiras linhas do mapeamento, com `passo` bytes por
/// linha). Devolve `None` quando o quadro não carrega a régua — que é o caso **normal** quando a
/// origem não é o gerador sintético, e por isso não é erro.
///
/// Lê só o **miolo** de cada bloco (a metade central), porque a borda é onde o filtro de
/// desbloqueio do H.264 mistura o bloco com o vizinho.
pub fn ler(luma: &[u8], passo: usize, largura: u32, altura: u32) -> Option<u32> {
    if (largura as usize) < DIGITOS * LADO || (altura as usize) < LADO {
        return None;
    }
    if passo < DIGITOS * LADO || luma.len() < passo * LADO {
        return None;
    }
    let borda = LADO / 4;
    let mut valor = 0u32;
    let mut peso = 1u32;
    for d in 0..DIGITOS {
        let mut soma: u64 = 0;
        let mut quantos: u64 = 0;
        for linha in borda..(LADO - borda) {
            let base = linha * passo + d * LADO;
            for x in borda..(LADO - borda) {
                soma += luma[base + x] as u64;
                quantos += 1;
            }
        }
        if quantos == 0 {
            return None;
        }
        let dig = digito((soma / quantos) as i32)?;
        valor += dig * peso;
        peso *= 4;
    }
    Some(valor)
}

/// **Escreve a régua** do quadro `n` no plano de luma (as `LADO` primeiras linhas, `passo` bytes por
/// linha): quatro blocos chapados de 64×64 no canto superior esquerdo, o dígito `d` de `n % 256` na
/// base 4 com a luma de [`luma_do_digito`], o menos significativo à esquerda. É o escritor que
/// faltava em Rust (`docs/camera-no-windows.md` §7.3): o mesmo desenho de `gerar-fonte.swift`, e o
/// teste o confere contra [`ler`], que foi provado contra aquele gerador.
///
/// Devolve `false` sem escrever nada quando o plano não comporta os quatro blocos.
pub fn escrever(n: u32, luma: &mut [u8], passo: usize) -> bool {
    if passo < DIGITOS * LADO || luma.len() < passo * LADO {
        return false;
    }
    let mut resto = n % MODULO;
    for d in 0..DIGITOS {
        let tom = luma_do_digito(resto % 4);
        resto /= 4;
        for linha in 0..LADO {
            let base = linha * passo + d * LADO;
            luma[base..base + LADO].fill(tom);
        }
    }
    true
}

/// O que a leitura da régua apurou ao longo de uma sessão.
///
/// Três contagens e não uma, porque elas respondem a perguntas diferentes: quadro **sem** régua é
/// "esta origem não é a sintética"; régua **ilegível** é "é a sintética e o caminho de pixel
/// estragou o bloco (ou a constante divergiu)"; e **fora de sequência** é "os pixels estão certos
/// mas os quadros não chegaram na ordem".
#[derive(Default, Clone, Copy, Debug)]
pub struct Contagem {
    pub lidas: u64,
    pub sem_regua: u64,
    pub ilegiveis: u64,
    pub em_sequencia: u64,
    pub fora_de_sequencia: u64,
    ultimo: Option<u32>,
    pub primeiro: Option<u32>,
    pub ultimo_visto: Option<u32>,
}

impl Contagem {
    /// Registra uma leitura e diz se ela seguiu a anterior.
    ///
    /// "Seguiu" é `(anterior + 1) % 256`, e a conta é módulo de propósito: a régua dá a volta a
    /// cada 256 quadros, e um leitor que não soubesse disso acusaria uma anomalia por volta.
    pub fn registrar(&mut self, valor: Option<u32>, tem_regua_possivel: bool) {
        match valor {
            Some(v) => {
                self.lidas += 1;
                if self.primeiro.is_none() {
                    self.primeiro = Some(v);
                }
                if let Some(anterior) = self.ultimo {
                    if (anterior + 1) % MODULO == v {
                        self.em_sequencia += 1;
                    } else {
                        self.fora_de_sequencia += 1;
                    }
                }
                self.ultimo = Some(v);
                self.ultimo_visto = Some(v);
            }
            None if tem_regua_possivel => self.ilegiveis += 1,
            None => self.sem_regua += 1,
        }
    }

    pub fn linha(&self) -> String {
        format!(
            "regua: lidas={} em_sequencia={} fora_de_sequencia={} ilegiveis={} sem_regua={} \
             primeira={} ultima={}",
            self.lidas,
            self.em_sequencia,
            self.fora_de_sequencia,
            self.ilegiveis,
            self.sem_regua,
            self.primeiro.map(|v| v.to_string()).unwrap_or_else(|| "—".into()),
            self.ultimo_visto.map(|v| v.to_string()).unwrap_or_else(|| "—".into()),
        )
    }
}

// ---------------------------------------------------------------------------------------------
// A leitura do lado da GPU
// ---------------------------------------------------------------------------------------------

/// Copia o canto do quadro decodificado para a CPU e lê a régua dali.
///
/// Fica aqui, e não em `exibicao.rs`, porque **as duas coisas que decodificam neste pacote
/// precisam dela**: o app (que recebe pela rede) e a sonda `quall-receiver-probe` (que lê de
/// arquivo). E é justamente a sonda que prova o leitor: ali o índice do quadro alimentado é
/// conhecido, então a conferência é "o número lido é exatamente `indice % 256`?" e não só "a
/// sequência anda". Um leitor provado por arquivo, no mesmo decoder e no mesmo hardware, é o que
/// permite confiar no número quando a origem passar a ser a rede.
#[cfg(windows)]
mod gpu {
    use anyhow::{anyhow, Context};
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};

    /// 256×64 de NV12 são 24 KiB por quadro, contra ~1,4 MiB de um 720p inteiro. A diferença
    /// importa porque esta cópia entra no caminho que se está medindo: copiar o quadro todo para
    /// ler quatro médias poluiria a própria latência do relatório.
    ///
    /// O caminho largo existe como piso, porque `CopySubresourceRegion` com caixa sobre um formato
    /// de vídeo tem regras de alinhamento que variam por driver — e um piso **silencioso** seria
    /// pior que um piso registrado.
    pub struct Leitor {
        dispositivo: ID3D11Device,
        contexto: ID3D11DeviceContext,
        estagio: ID3D11Texture2D,
        largura: u32,
        altura: u32,
        caminho_largo: bool,
    }

    impl Leitor {
        pub fn novo(
            dispositivo: &ID3D11Device,
            largura_do_quadro: u32,
            altura_do_quadro: u32,
        ) -> anyhow::Result<Self> {
            let contexto = unsafe { dispositivo.GetImmediateContext() }?;
            let l = ((super::DIGITOS * super::LADO) as u32).min(largura_do_quadro);
            let a = (super::LADO as u32).min(altura_do_quadro);
            let estagio = Self::criar(dispositivo, l, a)?;
            Ok(Self {
                dispositivo: dispositivo.clone(),
                contexto,
                estagio,
                largura: l,
                altura: a,
                caminho_largo: false,
            })
        }

        fn criar(
            dispositivo: &ID3D11Device,
            largura: u32,
            altura: u32,
        ) -> anyhow::Result<ID3D11Texture2D> {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: largura,
                Height: altura,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut textura: Option<ID3D11Texture2D> = None;
            unsafe { dispositivo.CreateTexture2D(&desc, None, Some(&mut textura)) }
                .context("criar a textura de estágio da régua")?;
            textura.ok_or_else(|| anyhow!("CreateTexture2D não devolveu textura de estágio"))
        }

        pub fn no_caminho_largo(&self) -> bool {
            self.caminho_largo
        }

        /// `Ok(None)` = a régua não está legível neste quadro (o caso normal quando a origem não é
        /// a fonte sintética). `Err` = a cópia da GPU falhou, que é problema diferente.
        pub fn ler(
            &mut self,
            textura: &ID3D11Texture2D,
            subrecurso: u32,
        ) -> anyhow::Result<Option<u32>> {
            unsafe {
                let caixa = D3D11_BOX {
                    left: 0,
                    top: 0,
                    front: 0,
                    right: self.largura,
                    bottom: self.altura,
                    back: 1,
                };
                let recorte = if self.caminho_largo { None } else { Some(&caixa as *const _) };
                self.contexto.CopySubresourceRegion(
                    &self.estagio,
                    0,
                    0,
                    0,
                    0,
                    textura,
                    subrecurso,
                    recorte,
                );

                let mut mapeado = D3D11_MAPPED_SUBRESOURCE::default();
                self.contexto
                    .Map(&self.estagio, 0, D3D11_MAP_READ, 0, Some(&mut mapeado))
                    .context("mapear a textura de estágio da régua")?;
                let passo = mapeado.RowPitch as usize;
                let linhas = self.altura as usize;
                let plano = std::slice::from_raw_parts(
                    mapeado.pData as *const u8,
                    passo.saturating_mul(linhas),
                );
                let valor = super::ler(plano, passo, self.largura, self.altura);
                self.contexto.Unmap(&self.estagio, 0);
                Ok(valor)
            }
        }

        /// Troca para o piso: cópia do quadro inteiro. Chamado uma vez, quando a caixa falha.
        pub fn cair_para_o_caminho_largo(
            &mut self,
            largura: u32,
            altura: u32,
        ) -> anyhow::Result<()> {
            crate::registro::linha(
                "regua: a cópia por caixa falhou — caindo para a cópia do quadro inteiro (mais \
                 cara, e ela entra na latência medida)",
            );
            self.estagio = Self::criar(&self.dispositivo, largura, altura)?;
            self.largura = largura;
            self.altura = altura;
            self.caminho_largo = true;
            Ok(())
        }
    }
}

#[cfg(windows)]
pub use gpu::Leitor;

#[cfg(test)]
mod testes {
    use super::*;

    /// Escreve a régua do jeito que `gerar-fonte.swift` escreve, para o teste medir o leitor
    /// contra a **especificação**, não contra si mesmo.
    fn pintar(n: u32, passo: usize, altura: usize, ruido: i32) -> Vec<u8> {
        let mut plano = vec![128u8; passo * altura];
        let mut resto = n % MODULO;
        for d in 0..DIGITOS {
            let dig = resto % 4;
            resto /= 4;
            let tom = luma_do_digito(dig) as i32;
            for linha in 0..LADO {
                for x in 0..LADO {
                    // O ruído entra só na borda: é onde o filtro de desbloqueio mexe de verdade,
                    // e o miolo é justamente o que o leitor promete usar.
                    let na_borda = linha < LADO / 4
                        || linha >= LADO - LADO / 4
                        || x < LADO / 4
                        || x >= LADO - LADO / 4;
                    let v = if na_borda { tom + ruido } else { tom };
                    plano[linha * passo + d * LADO + x] = v.clamp(0, 255) as u8;
                }
            }
        }
        plano
    }

    #[test]
    fn le_de_volta_o_que_o_gerador_escreveu() {
        for n in [0u32, 1, 3, 4, 17, 63, 64, 200, 255, 256, 257, 1000] {
            let plano = pintar(n, 512, 128, 0);
            assert_eq!(ler(&plano, 512, 512, 128), Some(n % MODULO), "n={n}");
        }
    }

    #[test]
    fn o_escritor_em_rust_e_lido_pelo_leitor() {
        // O escritor da câmera de bancada (fase 4) contra o leitor provado pelo gerador Swift, e
        // contra o pintor do teste, que segue a especificação: os dois dão o mesmo plano.
        for n in [0u32, 1, 2, 3, 4, 63, 64, 127, 200, 255, 256, 257, 1000, 65_535] {
            let mut plano = vec![200u8; 1920 * 80];
            assert!(escrever(n, &mut plano, 1920));
            assert_eq!(ler(&plano, 1920, 1920, 80), Some(n % MODULO), "n={n}");
            let mut do_pintor = pintar(n, 1920, 80, 0);
            // O pintor começa em 128 fora dos blocos; o escritor não mexe fora deles.
            for linha in 0..LADO {
                for x in DIGITOS * LADO..1920 {
                    do_pintor[linha * 1920 + x] = 200;
                }
            }
            for linha in LADO..80 {
                do_pintor[linha * 1920..(linha + 1) * 1920].fill(200);
            }
            assert_eq!(plano, do_pintor, "n={n}");
        }
        // Um plano pequeno demais não é escrito.
        let mut pequeno = vec![0u8; 200 * 64];
        assert!(!escrever(5, &mut pequeno, 200));
        assert!(pequeno.iter().all(|&v| v == 0));
    }

    #[test]
    fn o_miolo_sobrevive_a_borda_estragada() {
        // ±40 na borda é mais do que o desbloqueio faz e ainda assim o miolo decide.
        let plano = pintar(123, 512, 128, 40);
        assert_eq!(ler(&plano, 512, 512, 128), Some(123));
    }

    #[test]
    fn quadro_sem_regua_nao_vira_numero() {
        // Cinza chapado: 128 está a 38 níveis do 90 e a 12 do 140... e 12 < 21, então este caso
        // **seria** lido. É exatamente por isso que o teste usa um tom no meio de dois níveis.
        let plano = vec![115u8; 512 * 128];
        assert_eq!(ler(&plano, 512, 512, 128), None);
    }

    #[test]
    fn quadro_pequeno_demais_e_none_em_vez_de_panico() {
        let plano = vec![90u8; 200 * 40];
        assert_eq!(ler(&plano, 200, 200, 40), None);
        assert_eq!(ler(&plano, 200, 512, 128), None);
    }

    #[test]
    fn a_volta_dos_256_nao_conta_anomalia() {
        let mut c = Contagem::default();
        for n in 250..262u32 {
            c.registrar(Some(n % MODULO), true);
        }
        assert_eq!(c.lidas, 12);
        assert_eq!(c.fora_de_sequencia, 0, "a volta do módulo não é anomalia");
        assert_eq!(c.em_sequencia, 11);
    }

    #[test]
    fn um_salto_de_quadro_conta_anomalia() {
        let mut c = Contagem::default();
        c.registrar(Some(10), true);
        c.registrar(Some(11), true);
        c.registrar(Some(20), true);
        assert_eq!(c.fora_de_sequencia, 1);
        assert_eq!(c.em_sequencia, 1);
    }

    #[test]
    fn sem_regua_e_ilegivel_sao_contados_separado() {
        let mut c = Contagem::default();
        c.registrar(None, true);
        c.registrar(None, false);
        assert_eq!(c.ilegiveis, 1);
        assert_eq!(c.sem_regua, 1);
    }
}
