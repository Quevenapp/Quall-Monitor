//! Biblioteca compartilhada entre as duas sondas de bancada deste pacote:
//!
//! - `quall-capture-probe` (Frente 2, M1): captura de tela + encode H.264 em hardware.
//! - `quall-receiver-probe` (Frente 6, M2): decode H.264 em hardware + exibição numa janela.
//!
//! Só existe como `lib` porque as duas sondas compartilham `device.rs` (escolha de adaptador
//! DXGI) — a mesma armadilha medida no M1 (o `IMFDXGIDeviceManager` só aceita um dispositivo
//! criado no adaptador do MFT escolhido) vale tanto para o encoder quanto para o decoder, e
//! manter a lógica em dois lugares seria repetir o mesmo bug em dobro se um dia divergirem.

#[cfg(all(feature = "loja", feature = "tela-estendida-futura"))]
compile_error!("A build de distribuição não admite tela-estendida-futura.");

// Mesmo as mensagens antigas em stderr passam pela barreira; não altera a saída funcional da UI.
macro_rules! eprintln {
    ($($t:tt)*) => { crate::diagnostico_eprintln!($($t)*) };
}

#[cfg(windows)]
pub mod capture;
/// Redução da textura capturada ao teto do núcleo, na GPU. Ver o cabeçalho do módulo para por que
/// o Windows precisa disto e o macOS não.
#[cfg(windows)]
pub mod escala;
// Só existe com a feature `net` (default) ligada — é o único módulo deste pacote que toca
// `quall-core`/rede. Ver o comentário da feature em `Cargo.toml`.
#[cfg(feature = "net")]
pub mod connect;
#[cfg(windows)]
pub mod decoder;
#[cfg(windows)]
pub mod device;
#[cfg(windows)]
pub mod encoder;
/// A oficina que monta o encoder de reserva fora do laço — a metade de fundo da troca a quente.
/// Não depende de rede: só de `encoder.rs`.
#[cfg(windows)]
pub mod oficina;
#[cfg(windows)]
pub mod present;
pub mod sidecar;

// --- o app de produto (`quall-app`) ---
//
// `sps` e `fontes` não dependem de rede e ficam sempre disponíveis: o parser de SPS é útil a
// qualquer binário deste pacote, e o catálogo de monitores é o que a sonda de captura precisaria
// para deixar de ser presa ao monitor primário.
#[cfg(windows)]
pub mod fontes;

pub mod registro;
pub mod higiene_do_registro;
pub mod diagnostico_json;
pub mod diagnostico_cli;
#[cfg(feature = "net")]
pub mod diagnostico_rede;
// As câmeras do PC como origem (Frente C, `docs/camera-no-windows.md`): a regra do dono e da
// escolha é aritmética (`catalogo_de_cameras`, testes em qualquer máquina); a enumeração e a leitura
// do registro são Win32 (`cameras`). Nenhum dos dois toca rede nem ativa câmera.
#[cfg(windows)]
pub mod cameras;
pub mod catalogo_de_cameras;
// A captura da câmera (fase 3): a regra aritmética (o tipo nativo, o carimbo, o ritmo), a captura
// pelo leitor assíncrono do Media Foundation, e a conversão na GPU para o NV12 limitado do encoder.
// Nenhum dos três toca rede, então a sonda de bancada (sem `net`) usa os mesmos.
pub mod regras_da_camera;
pub mod captura_de_camera;
// **Os controles de câmera do R9** (`docs/controles-de-camera.md`): as regras puras (a faixa, o
// obturador em log2, o registro e o plano, testados em qualquer máquina).
pub mod regras_dos_controles;
pub mod modelo_dos_ajustes;
// **O controle remoto da câmera, R9b** (`docs/controle-remoto-da-camera.md`): as regras puras de
// quem filma (as capacidades, o pedido em gestos, o lido) e o painel de quem recebe, desenhado das
// capacidades que chegam. Testados em qualquer máquina; quem fala com o núcleo é `camera_remota`.
pub mod regras_da_camera_remota;
pub mod modelo_dos_ajustes_remotos;
// A thread que fala com o driver (`IAMCameraControl`, `IAMVideoProcAmp`, `IKsControl`) e a luma de
// bancada (`--luma-media`). Win32, sem rede.
pub mod ajustes_da_camera;
pub mod luma_de_bancada;
// A ponte do controle remoto da câmera (R9b) com o núcleo: o filmador de cada captura, a bombeada
// das sessões de vídeo, e o controle de quem recebe.
#[cfg(feature = "net")]
pub mod camera_remota;
// **O R5 no Windows** (`docs/teleprompter-com-camera.md` §8.10): o throttling de energia desligado no
// processo, o dono da captura (a câmera aberta com a tela, com três leitores), o microfone, o gravador
// local, e as regras puras deles (testadas em qualquer máquina).
pub mod energia;
pub mod regras_r5;
pub mod regras_da_gravacao;
pub mod dono_da_captura;
pub mod previa_da_camera;
pub mod gravador_local;
pub mod conversor_de_camera;
pub mod desentrelacador;

// --- a câmera virtual do Windows: o app é o **host** do cano ---
//
// Os três vieram de `integrations/camera-windows/sonda/src/` em 09/09/2026, e a mudança de casa é
// o que `receber.rs` daquela sonda já anunciava na primeira linha: *"este é o papel do app do
// Quall no desktop"*. A sonda continua usando os três — agora daqui, por `pub use`, em vez de ter
// a própria cópia.
//
// `escala_nv12` **não** é `escala`: aquele reduz a textura capturada ao teto do núcleo (RGB, no
// caminho de quem emite); este encaixa o quadro decodificado em 1920x1080 NV12 para o cano (no
// caminho de quem recebe). Nomes parecidos, lados opostos do produto — e por isso o de lá ganhou
// sufixo em vez de sobrescrever o daqui.
#[cfg(windows)]
pub mod cano;
#[cfg(windows)]
pub mod escala_nv12;
#[cfg(windows)]
pub mod placa;
// A baia: uma câmera virtual com o cano que a alimenta. Depende de `cano` e `placa`, e é o
// que o app cria uma vez por aparelho pareado.
#[cfg(windows)]
pub mod baia;
// O registro de baias: qual aparelho tem qual câmera. Ver o cabeçalho do módulo.
#[cfg(windows)]
pub mod baias;
pub mod sps;
// A régua de blocos lida do quadro decodificado. Não depende de rede nem de Windows — é
// aritmética sobre um plano de luma — e por isso os testes dela rodam em qualquer máquina.
pub mod regua;
pub mod regua_de_bancada;
// A condenação da cadeia de referência e a derivada do dano do enlace. Como a régua, os dois são
// aritmética pura: nada de Win32, nada de `quall-core`, nada de rede. É o que os torna
// exercitáveis por teste de unidade sem aparelho — a única prova que uma frente sem bancada tem
// como produzir sobre eles. Ver o cabeçalho de cada um para por que a peça mora na casca.
pub mod cadeia;
pub mod etapas;
pub mod fluidez;
pub mod janela_do_enlace;
// A origem da apresentação na janela, quadro a quadro: aritmética pura, pelo mesmo motivo (o G1 da
// troca de tamanho no meio da sessão, 21/09).
pub mod geometria_da_exibicao;

// --- o emissor com vários receptores (F2b) ---
//
// A tabela de sessões e a de índices são aritmética pura, como a régua: os testes delas rodam sem
// aparelho, e são a prova das regras do Mac portadas. A abstração de monitor tem a parte pura (o
// formato do monitor para a tela do receptor) e as implementações provisórias; a origem sintética é
// a textura nossa que a prova de mecanismo usa no lugar da tela.
pub mod monitor;
pub mod sessoes;
pub mod sintetica;
pub mod tabela_de_indices;

// --- o monitor virtual (SudoVDA, só a bancada; `docs/monitor-virtual-windows.md` §14) ---
//
// `ativacao` é a decisão pura da receita do §13 (tabelas de caminhos sintéticas nos testes, sem
// Windows); `sudovda` é o contrato do driver; `monitores_virtuais` é a `FonteDeMonitor` com o fio
// dono da topologia; `cobertura` é a janela sintética da bancada que cobre cada monitor. Nenhum
// toca rede: ficam fora do `net`.
#[cfg(feature = "tela-estendida-futura")]
pub mod ativacao;
#[cfg(feature = "tela-estendida-futura")]
pub mod cobertura;
#[cfg(feature = "tela-estendida-futura")]
pub mod monitores_virtuais;
#[cfg(feature = "tela-estendida-futura")]
pub mod sudovda;
// A thread de cada sessão e o coordenador tocam o núcleo com transporte.
#[cfg(feature = "net")]
pub mod sessao_de_emissao;
#[cfg(feature = "net")]
pub mod varias;

#[cfg(feature = "net")]
pub mod argumentos;
// O som do sistema por WASAPI loopback. Fica atrás de `net` porque quem codifica áudio é o app de
// produto — as duas sondas de bancada não têm track nenhuma, e sem `net` o `quall-opus` (libopus
// compilada do fonte) nem entra no grafo.
#[cfg(feature = "net")]
pub mod audio;
// O microfone do R5 (a peça 4 do §8.10): WASAPI de captura, Opus pelo núcleo. Atrás de `net` pelo
// `quall-opus` e pelo preset do núcleo, como `audio`.
#[cfg(feature = "net")]
pub mod microfone;
#[cfg(feature = "net")]
pub mod descoberta;
#[cfg(feature = "net")]
pub mod emissor;
#[cfg(feature = "net")]
pub mod enderecos;
// A metade que **exibe**: track → decoder em hardware → Video Processor → janela.
#[cfg(feature = "net")]
pub mod exibicao;
#[cfg(feature = "net")]
pub mod receptor;
pub mod som_puxado;
// A linha do tempo do som do loopback (`docs/som-no-receptor.md` §19.2): aritmética pura sobre as
// horas que o motor de áudio carimba, fora do `net` e sem Win32, para os testes rodarem em
// qualquer máquina. Quem a usa é `audio.rs`.
pub mod linha_do_loopback;
// O reamostrador (sinc com janela de Kaiser, tabela polifásica) e a disciplina da deriva
// (`som-no-receptor.md` §19.6): puros, como a linha, e testados em qualquer máquina.
pub mod reamostrador_sinc;
pub mod disciplina;
#[cfg(feature = "net")]
pub mod tocador;
#[cfg(feature = "net")]
pub mod identidade;
#[cfg(feature = "net")]
pub mod janela;
// **A janela "Ajustes da câmera"** (R9): a coluna das abas, com a prévia na câmera comum.
#[cfg(feature = "net")]
pub mod janela_dos_ajustes;
// **A bandeja** (01/10): o ícone na área de notificação, para onde a janela vai ao minimizar, com as
// sessões vivas. As regras (a dica, o menu, os avisos do ícone, o que minimizar faz) são puras e
// testadas em qualquer máquina; o Win32 (`bandeja`) mora com a janela, atrás de `net` como ela (a
// marca do primeiro aviso fica na pasta de dados de `identidade`).
#[cfg(feature = "net")]
pub mod bandeja;
pub mod regras_da_bandeja;
// Um Quall de produto por sessão: a segunda abertura pelo atalho chama a janela da primeira (da
// bandeja ou não) e sai sem abrir nada. Só na abertura de produto (sem argumentos).
#[cfg(feature = "net")]
pub mod instancia;
// **As peças e o modelo da janela** (`docs/telas-estudio.md` §4, §7): tokens, tabela de lugares,
// peças como listas de desenho, e a composição de cada cena a partir de um estado em valores
// simples. A metade pura dos dois não toca Win32 nem rede, e os testes dela rodam em qualquer
// máquina; o desenho (`estilo::d2d`) é Windows.
pub mod estilo;
/// A tradução EN/PT (02/10): a tabela português → inglês e o idioma de agora.
pub mod idioma;
pub mod opcoes_do_monitor;
pub mod modelo_da_janela;
// **A tela estendida no produto** (R10, 02/10): o SudoVDA que a pessoa instalar, detectado e nunca
// embutido. As regras (o ladrilho apagado, quem espelha) são puras, testadas em qualquer máquina.
#[cfg(feature = "tela-estendida-futura")]
pub mod regras_da_tela_estendida;
// **O driver da tela estendida que o Quall instala** (02/10, noite, `docs/monitor-virtual-windows.md`
// §15): as regras puras (hashes, o pedido ao processo elevado, o protocolo do cano, a situação) e o
// Win32 (o processo elevado, a caixa, o lançamento). Sem rede: fora do `net`.
#[cfg(all(windows, feature = "tela-estendida-futura"))]
pub mod driver_da_tela_estendida;
#[cfg(feature = "tela-estendida-futura")]
pub mod regras_do_driver;
#[cfg(feature = "net")]
pub mod transmissao;

// --- o teleprompter (F6, os dois papéis) ---
//
// A thread da sessão e as regras não são Win32 e são provadas por teste (inclusive com sessões de
// verdade por 127.0.0.1); a janela e o desenho são. Atrás de `net` porque a réplica e a sessão são
// o núcleo com transporte. Ver o cabeçalho de `teleprompter/mod.rs`.
#[cfg(feature = "net")]
pub mod teleprompter;
