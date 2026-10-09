//! O lado servidor do cano de quadros (o papel que, no produto, é do app do Quall).
//!
//! Ver `fonte/src/quadros.rs` para o porquê de ser cano nomeado e de o **host** ser o servidor.
//!
//! Aqui há mais uma coisa, e ela é **de bancada, desligada por padrão**: sob
//! `--carimbo-de-bancada`, cada quadro leva um **carimbo de QPC** escrito nos próprios pixels, nas
//! oito primeiras colunas das oito primeiras linhas do plano Y. `QueryPerformanceCounter` é um
//! relógio da máquina inteira, igual em qualquer processo — então o consumidor consegue subtrair e
//! obter a latência host→app **atravessando o Frame Server**, que é a única medida que interessa.
//! Um `Instant` do Rust não serviria: ele não é comparável entre processos.
//!
//! Sem a opção, o quadro entregue ao app consumidor é **só imagem**. Ver [`CARIMBO_ARMADO`] para
//! por que a chave mora dentro de [`carimbar`] em vez de num parâmetro.

use anyhow::{Context, Result};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{WriteFile, PIPE_ACCESS_OUTBOUND};
use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_WAIT};

// **Uma casa só para o contrato do cano.** A geometria, o nome sem-nome e a derivação do nome
// pelo nome da câmera vêm da fonte de mídia, que é quem lê do outro lado. Até 09/09/2026 estes
// quatro valores estavam escritos aqui **e** em `fonte/src/quadros.rs`, e divergir era questão de
// tempo: o sintoma seria a fonte esperando 3,11 MB de um host que escreve 1,38 MB, ou seja imagem
// parada sem erro nenhum.
pub use quall_camera_fonte::quadros::{cano_do_nome, ALTURA, BYTES_NV12, CANO_SEM_NOME, FPS, LARGURA};

/// O cano de quem não tem nome. Nome curto porque é o que os roteiros de bancada já escrevem.
pub const CANO: &str = CANO_SEM_NOME;
const MAGICO: u32 = 0x4C41_5551;

/// DACL do cano, em SDDL:
/// - `SY` (LocalSystem) e `LS` (LocalService): o Frame Server roda como LocalService e o Frame
///   Server Monitor como LocalSystem. **Sem estas duas ACEs o cano existe e a fonte nunca
///   conecta** — e o sintoma é imagem parada, não erro.
/// - `AU` (usuários autenticados): a fonte também é instanciada dentro do processo do app
///   consumidor, que roda como o usuário.
///
/// É frouxa de propósito e o custo está registrado no README: qualquer processo local autenticado
/// consegue ler o vídeo recebido. Para uma ferramenta de LAN sem conta nem login, o mesmo vídeo já
/// está na tela; apertar isso aqui é trabalho do M6, não desta entrega.
const SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;LS)(A;;GRGW;;;AU)";

pub fn qpc_us() -> u64 {
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    let mut f = 0i64;
    let mut c = 0i64;
    unsafe {
        let _ = QueryPerformanceFrequency(&mut f);
        let _ = QueryPerformanceCounter(&mut c);
    }
    if f == 0 {
        return 0;
    }
    (c as u128 * 1_000_000 / f as u128) as u64
}

/// Estado do carimbo de bancada, **desarmado por padrão**.
///
/// # Por que a chave mora aqui dentro, e não no ponto de chamada
///
/// Este carimbo é instrumento de bancada: ele escreve oito bytes de relógio nos pixels que o app
/// consumidor recebe, o que vira uma marca de 8x8 no canto superior esquerdo do que sai para o
/// Zoom, o Meet ou o OBS de quem usar. Ele **vazou para o produto** e ficou lá por três rodadas,
/// documentado como dívida e nunca consertado, porque a chamada era incondicional e nada no tipo
/// nem na assinatura lembrava disso.
///
/// Passar um `bool` por parâmetro consertaria os dois pontos de chamada de hoje e não protegeria o
/// terceiro. Com a chave **dentro** de `carimbar`, um ponto de chamada novo nasce desarmado: o
/// modo de falhar passa a ser "faltou número na bancada", que aparece na hora, em vez de
/// "instrumento na imagem do usuário", que não aparece nunca.
///
/// Quem arma é `main.rs`, uma vez, e só sob `--carimbo-de-bancada`.
static CARIMBO_ARMADO: AtomicBool = AtomicBool::new(false);

/// Arma o carimbo de bancada para o resto da vida deste processo. Só `main.rs` chama, e só quando
/// a opção de linha de comando pede.
pub fn armar_carimbo_de_bancada() {
    CARIMBO_ARMADO.store(true, Ordering::Relaxed);
}

/// O carimbo está armado neste processo? Serve para o relatório declarar o próprio estado — a
/// prova de que ele está desligado no caminho normal sai daqui e do contador do consumidor, não
/// de ler o código.
pub fn carimbo_armado() -> bool {
    CARIMBO_ARMADO.load(Ordering::Relaxed)
}

/// Escreve o carimbo nos pixels — **se, e só se, ele estiver armado**. Devolve se escreveu, para
/// quem chama poder contar e declarar.
///
/// Oito linhas iguais: se o consumidor ler as oito idênticas, é prova de que ninguém reamostrou a
/// imagem no caminho — e se ler diferentes, a medida de latência estaria mentindo e é melhor
/// saber.
pub fn carimbar(buf: &mut [u8], us: u64) -> bool {
    if !CARIMBO_ARMADO.load(Ordering::Relaxed) {
        return false;
    }
    let b = us.to_le_bytes();
    for linha in 0..8usize {
        let base = linha * LARGURA as usize;
        buf[base..base + 8].copy_from_slice(&b);
    }
    true
}

pub struct Servidor {
    handle: HANDLE,
}

impl Servidor {
    /// Cria uma instância do cano e **espera** um cliente conectar.
    /// `cano` é o nome completo (`\\\\.\\pipe\\...`). Vem de [`cano_do_nome`] quando a câmera tem
    /// nome, e de [`CANO_SEM_NOME`] quando não tem.
    pub fn esperar_cliente(cano: &str) -> Result<Servidor> {
        unsafe {
            let mut sd = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(HSTRING::from(SDDL).as_ptr()),
                1, // SDDL_REVISION_1
                &mut sd,
                None,
            )
            .context("SDDL do cano")?;

            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0 as *mut c_void,
                bInheritHandle: false.into(),
            };

            let h = CreateNamedPipeW(
                PCWSTR(HSTRING::from(cano).as_ptr()),
                PIPE_ACCESS_OUTBOUND,
                PIPE_TYPE_BYTE | PIPE_WAIT,
                8, // instâncias: Frame Server, app consumidor, e folga
                (BYTES_NV12 * 2) as u32,
                0,
                0,
                Some(&sa),
            );
            if h.is_invalid() {
                anyhow::bail!(
                    "CreateNamedPipeW falhou: {:?}",
                    windows::Win32::Foundation::GetLastError()
                );
            }
            // `ConnectNamedPipe` devolvendo ERROR_PIPE_CONNECTED significa que o cliente chegou
            // entre o Create e o Connect. É sucesso, não erro — tratar como erro aqui é o jeito
            // clássico de perder o primeiro consumidor.
            if let Err(e) = ConnectNamedPipe(h, None) {
                if e.code() != ERROR_PIPE_CONNECTED.to_hresult() {
                    let _ = CloseHandle(h);
                    return Err(e).context("ConnectNamedPipe");
                }
            }
            Ok(Servidor { handle: h })
        }
    }

    /// Escreve um quadro. Devolve o custo da escrita em microssegundos.
    pub fn escrever(&self, corpo: &[u8], ts_us: u64) -> Result<u64> {
        let mut cab = Vec::with_capacity(24);
        cab.extend_from_slice(&MAGICO.to_le_bytes());
        cab.extend_from_slice(&LARGURA.to_le_bytes());
        cab.extend_from_slice(&ALTURA.to_le_bytes());
        cab.extend_from_slice(&(corpo.len() as u32).to_le_bytes());
        cab.extend_from_slice(&ts_us.to_le_bytes());

        let t0 = qpc_us();
        unsafe {
            let mut escritos = 0u32;
            WriteFile(self.handle, Some(&cab), Some(&mut escritos), None)?;
            WriteFile(self.handle, Some(corpo), Some(&mut escritos), None)?;
        }
        Ok(qpc_us() - t0)
    }
}

impl Drop for Servidor {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}
