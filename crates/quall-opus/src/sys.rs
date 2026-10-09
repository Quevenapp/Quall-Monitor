//! As declarações da API C da libopus, escritas à mão.
//!
//! # Por que à mão e não por `bindgen`
//!
//! São **doze** funções e vinte constantes. Um `bindgen` no build custaria libclang no caminho de
//! compilação do nosso código e teria de receber o sysroot certo para o `armv7-linux-androideabi`
//! — máquinas a mais para gerar o que cabe nesta tela. É a mesma escolha que o `quall-rtc` já fez
//! contra o crate seguro `datachannel`: quando a superfície é pequena, escrevê-la é mais barato e
//! muito mais legível que gerá-la.
//!
//! **Todos os números abaixo foram lidos de `vendor/opus/include/opus_defines.h`**, não de
//! memória. Se a libopus vendorizada subir de versão, releia-os de lá: eles são ABI, e um valor
//! errado aqui não dá erro de compilação — dá um `opus_encoder_ctl` que configura outra coisa em
//! silêncio.

use std::os::raw::{c_char, c_int};

/// O estado do codificador. Opaco de propósito: o tamanho dele vem de
/// `opus_encoder_get_size` e não do nosso lado.
pub enum OpusEncoder {}
/// O estado do decodificador. Idem.
pub enum OpusDecoder {}

// --- Códigos de retorno (opus_defines.h:46-60) ---------------------------------------------
pub const OPUS_OK: c_int = 0;
pub const OPUS_BAD_ARG: c_int = -1;
pub const OPUS_BUFFER_TOO_SMALL: c_int = -2;
pub const OPUS_INTERNAL_ERROR: c_int = -3;
pub const OPUS_INVALID_PACKET: c_int = -4;
pub const OPUS_UNIMPLEMENTED: c_int = -5;
pub const OPUS_INVALID_STATE: c_int = -6;
pub const OPUS_ALLOC_FAIL: c_int = -7;

// --- Pedidos de `opus_encoder_ctl` (opus_defines.h:130-171) --------------------------------
pub const OPUS_SET_BITRATE_REQUEST: c_int = 4002;
pub const OPUS_SET_MAX_BANDWIDTH_REQUEST: c_int = 4004;
pub const OPUS_SET_VBR_REQUEST: c_int = 4006;
pub const OPUS_SET_BANDWIDTH_REQUEST: c_int = 4008;
pub const OPUS_SET_COMPLEXITY_REQUEST: c_int = 4010;
pub const OPUS_SET_INBAND_FEC_REQUEST: c_int = 4012;
pub const OPUS_SET_PACKET_LOSS_PERC_REQUEST: c_int = 4014;
pub const OPUS_SET_DTX_REQUEST: c_int = 4016;
pub const OPUS_SET_FORCE_CHANNELS_REQUEST: c_int = 4022;
pub const OPUS_SET_SIGNAL_REQUEST: c_int = 4024;
pub const OPUS_GET_LOOKAHEAD_REQUEST: c_int = 4027;

// --- Valores (opus_defines.h:205-224) ------------------------------------------------------
pub const OPUS_AUTO: c_int = -1000;
pub const OPUS_APPLICATION_VOIP: c_int = 2048;
pub const OPUS_APPLICATION_AUDIO: c_int = 2049;
pub const OPUS_APPLICATION_RESTRICTED_LOWDELAY: c_int = 2051;
pub const OPUS_SIGNAL_VOICE: c_int = 3001;
pub const OPUS_SIGNAL_MUSIC: c_int = 3002;
pub const OPUS_BANDWIDTH_NARROWBAND: c_int = 1101;
pub const OPUS_BANDWIDTH_MEDIUMBAND: c_int = 1102;
pub const OPUS_BANDWIDTH_WIDEBAND: c_int = 1103;
pub const OPUS_BANDWIDTH_SUPERWIDEBAND: c_int = 1104;
pub const OPUS_BANDWIDTH_FULLBAND: c_int = 1105;

extern "C" {
    pub fn opus_encoder_create(
        fs: i32,
        canais: c_int,
        aplicacao: c_int,
        erro: *mut c_int,
    ) -> *mut OpusEncoder;
    pub fn opus_encoder_destroy(st: *mut OpusEncoder);
    pub fn opus_encode(
        st: *mut OpusEncoder,
        pcm: *const i16,
        amostras_por_canal: c_int,
        dados: *mut u8,
        maximo_de_bytes: i32,
    ) -> i32;
    /// Variádica em C. Todo pedido `OPUS_SET_*` desta caixa passa **um** `c_int` por valor.
    pub fn opus_encoder_ctl(st: *mut OpusEncoder, pedido: c_int, ...) -> c_int;

    pub fn opus_decoder_create(fs: i32, canais: c_int, erro: *mut c_int) -> *mut OpusDecoder;
    pub fn opus_decoder_destroy(st: *mut OpusDecoder);
    /// `dados` nulo pede ocultação de perda. `decodificar_fec` diferente de zero pede que o
    /// decoder tire do LBRR deste pacote o quadro **anterior**, que é o recurso inteiro do FEC.
    pub fn opus_decode(
        st: *mut OpusDecoder,
        dados: *const u8,
        tamanho: i32,
        pcm: *mut i16,
        amostras_por_canal: c_int,
        decodificar_fec: c_int,
    ) -> c_int;

    // --- Leitura de pacote, sem estado. É com estas que a prova do TOC é conferida contra o
    // --- parser do próprio upstream, e não só contra o nosso.
    pub fn opus_packet_get_bandwidth(dados: *const u8) -> c_int;
    pub fn opus_packet_get_nb_frames(pacote: *const u8, tamanho: i32) -> c_int;
    pub fn opus_packet_get_samples_per_frame(dados: *const u8, fs: i32) -> c_int;
    pub fn opus_packet_get_nb_channels(dados: *const u8) -> c_int;
    /// Diz se o pacote carrega LBRR — a cópia de baixa taxa do quadro anterior, que **é** o FEC
    /// embutido do Opus. Ela percorre o cabeçalho SILK com o decodificador de faixa de verdade;
    /// não dá para ler isto do TOC.
    pub fn opus_packet_has_lbrr(pacote: *const u8, tamanho: i32) -> c_int;

    pub fn opus_strerror(erro: c_int) -> *const c_char;
    pub fn opus_get_version_string() -> *const c_char;
}

/// A `unsafe impl Send` das duas caixas mora aqui para ficar ao lado do motivo.
///
/// `OpusEncoder` e `OpusDecoder` são blocos de memória sem ponteiro global e sem estado
/// compartilhado: a libopus aloca tudo dentro do próprio estado. Mover um entre threads é seguro;
/// **usá-lo de duas ao mesmo tempo não é**, e é por isso que só `Send` é implementado, nunca
/// `Sync`. Toda função que mexe no estado pede `&mut self`.
pub(crate) mod marcadores {}
