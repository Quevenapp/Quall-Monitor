//! **A bancada do teleprompter**: a captura da própria janela, a área de transferência do editor,
//! e a gravação do relato final.
//!
//! # A captura é da janela deste processo, e só dela
//!
//! `docs/regras-de-frente.md`: nunca a tela inteira de nenhuma sessão do Dell — a sessão
//! interativa tem a vida do usuário. A captura pede à **nossa** janela que se desenhe num bitmap
//! nosso (`WM_PRINT`); nada de fora dela entra no arquivo. Na janela só há o que a corrida pôs
//! lá: o roteiro sintético, o PIN da bancada e o endereço.
//!
//! **`WM_PRINT`, e não `PrintWindow`**: medido na Sessão 0 em 14/09 — `PrintWindow` devolve
//! sucesso e um quadro inteiro preto, com e sem `PW_RENDERFULLCONTENT`, até para a janela GDI do
//! controle (fundo branco). Lá não há monitor nem DWM; `WM_PRINT` só pede à janela e aos botões
//! que se pintem no DC dado, sem passar pela tela.

use std::path::Path;

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowRect, SendMessageW, WM_PRINT};

/// `PRF_CLIENT | PRF_ERASEBKGND | PRF_CHILDREN` e `PRF_NONCLIENT` do `winuser.h`, escritos à mão
/// pelo motivo de `janela.rs` com `BST_CHECKED`: números que o cabeçalho da Microsoft fixou há
/// trinta anos.
const PRF_DA_CAPTURA: isize = 0x04 | 0x08 | 0x10;
const PRF_NONCLIENT: isize = 0x02;

/// **Um bitmap nosso, de 32 bits, de cima para baixo, num DC de memória.** É o quadro de trás do
/// prompter (o Direct2D desenha nele e um `BitBlt` o põe na janela) e o papel da captura.
pub struct Bitmap {
    pub dc: HDC,
    pub largura: i32,
    pub altura: i32,
    bitmap: HBITMAP,
    antigo: HGDIOBJ,
    bits: *mut core::ffi::c_void,
}

impl Bitmap {
    pub fn novo(largura: i32, altura: i32) -> Result<Bitmap, String> {
        if largura <= 0 || altura <= 0 {
            return Err("tamanho vazio".into());
        }
        unsafe {
            // Compatível com a tela, e com um DIB dentro: o GDI desenha nele pelo motor de DIB,
            // sem passar pelo driver de vídeo — é o que funciona também sem monitor (Sessão 0).
            let dc = CreateCompatibleDC(None);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: largura,
                    biHeight: -altura,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) => b,
                Err(e) => {
                    let _ = DeleteDC(dc);
                    return Err(format!("CreateDIBSection: {e}"));
                }
            };
            let antigo = SelectObject(dc, bitmap.into());
            Ok(Bitmap { dc, largura, altura, bitmap, antigo, bits })
        }
    }

    /// Os pixels (BGRA), depois de o GDI terminar o que tinha na fila.
    pub fn pixels(&self) -> Vec<u8> {
        unsafe {
            let _ = GdiFlush();
            if self.bits.is_null() {
                return Vec::new();
            }
            let n = (self.largura as usize) * (self.altura as usize) * 4;
            std::slice::from_raw_parts(self.bits as *const u8, n).to_vec()
        }
    }
}

impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.antigo);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Captura **a área de cliente da janela deste processo** para um BMP de 32 bits. Devolve o
/// tamanho e se veio alguma coisa além de preto (a testemunha de que a janela de fato se pintou).
///
/// A janela é impressa **inteira** (com a moldura, `PRF_NONCLIENT`) num bitmap do tamanho dela, e
/// a área de cliente é recortada depois: sem a moldura, o `WM_PRINT` põe os botões filhos na posição
/// deles **relativa à janela**, e não à área de cliente — medido na captura de 14/09, a barra de
/// baixo saiu deslocada da altura da barra de título, cortada na borda.
pub fn capturar_janela(hwnd: HWND, caminho: &Path) -> Result<(i32, i32, bool), String> {
    let mut cliente = RECT::default();
    unsafe { GetClientRect(hwnd, &mut cliente) }.map_err(|e| format!("GetClientRect: {e}"))?;
    let mut janela = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut janela) }.map_err(|e| format!("GetWindowRect: {e}"))?;
    let mut origem = POINT { x: 0, y: 0 };
    let _ = unsafe { ClientToScreen(hwnd, &mut origem) };
    let (ww, wh) = (janela.right - janela.left, janela.bottom - janela.top);
    let (dx, dy) = ((origem.x - janela.left).clamp(0, ww.max(0)), (origem.y - janela.top).clamp(0, wh.max(0)));
    let (cw, ch) = ((cliente.right - cliente.left).min(ww - dx), (cliente.bottom - cliente.top).min(wh - dy));
    if cw <= 0 || ch <= 0 {
        return Err(format!("janela sem área de cliente ({ww}x{wh})"));
    }
    let b = Bitmap::novo(ww, wh)?;
    unsafe {
        SendMessageW(hwnd, WM_PRINT, Some(WPARAM(b.dc.0 as usize)), Some(LPARAM(PRF_NONCLIENT | PRF_DA_CAPTURA)));
    }
    let tudo = b.pixels();
    if tudo.len() < (ww as usize) * (wh as usize) * 4 {
        return Err("o bitmap da captura veio sem pixels".into());
    }
    let mut pixels = Vec::with_capacity((cw as usize) * (ch as usize) * 4);
    for y in 0..ch {
        let comeco = (((y + dy) as usize) * (ww as usize) + dx as usize) * 4;
        pixels.extend_from_slice(&tudo[comeco..comeco + (cw as usize) * 4]);
    }
    let pintou = pixels.chunks_exact(4).any(|p| p[0] != 0 || p[1] != 0 || p[2] != 0);
    escrever_bmp(caminho, cw, ch, &pixels).map_err(|e| format!("gravar {}: {e}", caminho.display()))?;
    Ok((cw, ch, pintou))
}

/// Um BMP de 32 bits, de cima para baixo, sem compressão — o formato mais simples que qualquer
/// visualizador abre (no Mac, `sips -s format png` o converte).
fn escrever_bmp(caminho: &Path, w: i32, h: i32, bgra: &[u8]) -> std::io::Result<()> {
    let tamanho_da_imagem = bgra.len() as u32;
    let deslocamento = 14u32 + 40;
    let mut b = Vec::with_capacity(bgra.len() + deslocamento as usize);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&(deslocamento + tamanho_da_imagem).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&deslocamento.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&w.to_le_bytes());
    b.extend_from_slice(&(-h).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&tamanho_da_imagem.to_le_bytes());
    b.extend_from_slice(&2835i32.to_le_bytes());
    b.extend_from_slice(&2835i32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(bgra);
    if let Some(pai) = caminho.parent() {
        let _ = std::fs::create_dir_all(pai);
    }
    std::fs::write(caminho, b)
}

/// Põe `texto` na área de transferência. É para onde vai o rascunho descartado por "usar o texto
/// novo" — nada do que a pessoa digitou se perde sem ela saber.
pub fn copiar(hwnd: HWND, texto: &str) -> bool {
    let largo: Vec<u16> = texto.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let ok = match GlobalAlloc(GMEM_MOVEABLE, largo.len() * 2) {
            Ok(h) => {
                let p = GlobalLock(h) as *mut u16;
                if p.is_null() {
                    false
                } else {
                    std::ptr::copy_nonoverlapping(largo.as_ptr(), p, largo.len());
                    let _ = GlobalUnlock(h);
                    // Depois do `SetClipboardData` a memória é do sistema: não se libera aqui.
                    SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(h.0))).is_ok()
                }
            }
            Err(_) => false,
        };
        let _ = CloseClipboard();
        ok
    }
}

/// Grava um JSON por troca de arquivo (quem lê no meio nunca vê meio arquivo).
pub fn escrever_json(caminho: &Path, valor: &serde_json::Value) -> std::io::Result<()> {
    if let Some(pai) = caminho.parent() {
        let _ = std::fs::create_dir_all(pai);
    }
    let texto = serde_json::to_string_pretty(valor).unwrap_or_default();
    let provisorio = caminho.with_extension("json.gravando");
    std::fs::write(&provisorio, texto)?;
    std::fs::rename(&provisorio, caminho)
}
