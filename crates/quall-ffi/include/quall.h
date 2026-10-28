// Gerado por cbindgen a partir de crates/quall-ffi. Não editar à mão.
//
// =============================================================================================
// Convenções desta fronteira
// =============================================================================================
//
// - Toda string é UTF-8 terminada em NUL.
//
// - **Padrão `(buf, cap) -> intptr_t`.** A função devolve quantos bytes são necessários
//   *incluindo o NUL*, e só escreve se couber. Chame com `buf` nulo para perguntar o tamanho,
//   aloque, e chame de novo. Negativo é erro; o motivo sai em `quall_last_error()`.
//
// - Função que devolve ponteiro devolve **nulo** em erro; o motivo sai em `quall_last_error()`
//   como texto e em `quall_last_status()` como **código**.
//
// - `quall_last_error()` e `quall_last_status()` são **por thread** e valem até a próxima falha
//   na mesma thread. Leia-os logo depois da chamada que falhou, antes de qualquer outra função
//   `quall_`, e só quando ela de fato sinalizou falha (nulo, ou negativo) — uma chamada
//   bem-sucedida **não** limpa nenhum dos dois. Copie a mensagem se precisar guardar.
//
// - **Decida pelo código, não pelo texto.** A mensagem está em português e é para a pessoa;
//   comparar prefixo dela é exatamente o defeito que fez `QUALL_STATUS_NO_ROUTE` existir.
//   `QUALL_STATUS_NEEDS_PIN` depois de um `quall_connect` nulo não é recusa: é convite a
//   mostrar a tela de PIN de novo.
//
// - Todo ponteiro devolvido por `..._new`, `..._start`, `quall_host`, `quall_connect`,
//   `quall_session_track` e `quall_session_next_track` é do chamador e tem função de liberar.
//   Liberar duas vezes é erro do chamador, como em qualquer API C.
//
// - `quall_host`, `quall_connect` e `quall_browser_collect` **bloqueiam**. Chame de uma thread
//   de trabalho, nunca da thread de interface. Para interromper a espera de `quall_host` e
//   `quall_connect`, use `quall_canceller_new()` + `quall_host_cancelable()` e chame
//   `quall_session_cancel()` do botão Cancelar.
//
// - **Callbacks rodam em threads da libdatachannel**, não na sua. Não bloqueie dentro deles. No
//   Android essas threads **não estão anexadas à JVM**: prefira `quall_track_take_idr_request()`
//   ao callback.
//
// - **Para desregistrar um callback, chame a mesma função com `cb = NULL`.**
//   `quall_track_on_frame(t, NULL, NULL)` e `quall_track_on_idr_request(t, NULL, NULL)`
//   desligam o tratador. Com `QUALL_STATUS_OK`, o tratador antigo **não está rodando em thread
//   nenhuma** e não voltará a rodar: o `user_data` pode ser liberado, mesmo com a sessão de pé.
//   `quall_track_free()` **não** desregistra — dois handles podem apontar para a mesma track.
//
// - **`quall_session_close()` é barreira, e é ela que autoriza liberar o `user_data`.** Com
//   `QUALL_STATUS_OK`, ao voltar: nenhum callback desta sessão está rodando, e nenhum voltará a
//   rodar. Libere o `user_data` na linha seguinte, à vontade.
//
//   Status diferente de `QUALL_STATUS_OK` quer dizer **não libere nada** — a sessão foi fechada
//   e liberada de qualquer jeito, o que falhou foi a promessa sobre os callbacks:
//   `QUALL_STATUS_TIMEOUT` é um callback seu que não voltou em 2 s (callback não pode
//   bloquear); `QUALL_STATUS_INVALID` é você ter chamado **de dentro de um callback**, caso em
//   que esperar seria travar o processo — feche de outra thread.
//
// - **Não feche a sessão de dentro de um callback.** Além de não render barreira, destruir a
//   conexão de dentro de uma thread da libdatachannel é pedir para ela se enroscar. Levante uma
//   bandeira e feche do seu próprio laço.
//
// - Um objeto (`QuallSession`, `QuallBrowser`, `QuallTrack`) pode ser usado de mais de uma
//   thread, mas as funções que **avançam** estado — `quall_session_next_track`,
//   `quall_session_next_event`, `quall_browser_collect`, `quall_messages_next`,
//   `quall_teleprompter_pump` — devem ser chamadas de uma thread só.
//
// - **Mensagens entre aparelhos** (`quall_session_messages`, `docs/contrato-teleprompter.md`): o
//   handle `QuallMessages` sobrevive a `quall_session_close`, como o de track, e depois dele
//   devolve `QUALL_STATUS_CLOSED` sem tocar na biblioteca. `quall_messages_send` pode ser chamado
//   de **qualquer** thread e não bloqueia. `quall_messages_next` só tira a mensagem da fila
//   quando ela coube em `buf`: devolvido maior que `cap` quer dizer "não consumida, aloque e
//   chame de novo"; `0` quer dizer "nada chegou". Numa sessão, **um leitor só**: num
//   teleprompter quem lê é `quall_teleprompter_pump`.
//
// - **O teleprompter** (`QuallTeleprompter`): uma réplica por aparelho, que vive mais que a sessão.
//   As edições (`quall_teleprompter_set_*`, `_jump`, `_jump_by`) podem vir de qualquer thread e
//   saem na hora; a bombeada roda em laço na thread da sessão, junto com
//   `quall_session_next_event(s, 0)`. Os bits de `changed` são os de `QuallTeleprompterChange`.


#ifndef QUALL_H
#define QUALL_H

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

/**
 * Resultado de uma chamada. `QUALL_OK` é zero; todo o resto é falha.
 */
typedef enum QuallStatus {
    QUALL_STATUS_OK = 0,
    /**
     * Entrada malformada vinda do usuário ou da rede.
     */
    QUALL_STATUS_INVALID = 1,
    /**
     * A outra ponta fala outra versão do protocolo, ou mandou algo fora de ordem.
     */
    QUALL_STATUS_PROTOCOL = 2,
    QUALL_STATUS_DISCOVERY = 3,
    QUALL_STATUS_SIGNALING = 4,
    QUALL_STATUS_TRANSPORT = 5,
    /**
     * **Pareamento recusado por um motivo que não é o PIN nem o par esquecido.**
     *
     * MAC de retomada que não confere, segredo guardado do aparelho errado, mensagem fora de
     * ordem. **Dívida 29:** até 2026-08-27 este código também cobria os outros dois, e as cascas
     * escreviam "O PIN não conferiu" em cima dele — a frente do Windows viu esse texto **com o
     * PIN certo**. Quem quiser dar conselho ao usuário quer [`QuallStatus::WrongPin`] ou
     * [`QuallStatus::NeedsPin`]; este aqui é o resto, e o conselho honesto para ele é "tente de
     * novo, e se insistir, pareie do zero".
     */
    QUALL_STATUS_PAIRING = 6,
    QUALL_STATUS_TIMEOUT = 7,
    QUALL_STATUS_CLOSED = 8,
    QUALL_STATUS_IO = 9,
    /**
     * Ponteiro nulo onde a função exige um objeto.
     *
     * **Dívida 8.** Este código e o [`QuallStatus::NotUtf8`] estavam no header sem que nenhum
     * caminho os produzisse: ponteiro nulo saía como `QUALL_STATUS_INVALID`, misturado com PIN
     * malformado e IP inválido. Agora saem separados — erro de programação da casca de um lado,
     * entrada ruim do usuário do outro.
     */
    QUALL_STATUS_NULL_POINTER = 10,
    /**
     * String do chamador que não é UTF-8 válido. Ver [`QuallStatus::NullPointer`].
     */
    QUALL_STATUS_NOT_UTF8 = 11,
    /**
     * **O ICE não achou caminho entre os dois aparelhos.**
     *
     * No iOS é quase sempre **permissão de Rede Local negada**; nas outras plataformas, é
     * isolamento de AP ou Wi-Fi de hóspede. Existe porque a casca iOS distinguia este caso —
     * o mais provável do produto — comparando prefixo de string em português, que quebra na
     * primeira vez que alguém reescreve a mensagem.
     *
     * **Dívida 28, consertada em 2026-08-27.** Este código nascia **num lugar só**, o
     * `PeerState::Failed` do ICE — que só corre depois de a sinalização estar de pé. Quando não
     * há rota, quem morre primeiro é o `connect` TCP da sinalização, e aquilo saía como
     * `QUALL_STATUS_IO`: o status era inalcançável justamente no caso mais comum de não haver
     * rota, e o receptor iOS teve de contornar com uma sonda TCP própria. Agora nasce nas duas
     * camadas.
     *
     * **`QUALL_STATUS_IO` numa conexão recusada continua sendo `IO`, e de propósito**: um RST é
     * prova de que existe rota, e o conselho ali é "abra o app no outro aparelho".
     */
    QUALL_STATUS_NO_ROUTE = 12,
    /**
     * **Este aparelho não está mais pareado do outro lado: peça o PIN de novo.**
     *
     * Não é recusa, é convite a recomeçar — a casca mostra a tela de PIN em vez de "falhou".
     * Ver a dívida 22.
     */
    QUALL_STATUS_NEEDS_PIN = 13,
    /**
     * A casca cancelou a espera com [`quall_session_cancel`].
     */
    QUALL_STATUS_CANCELLED = 14,
    /**
     * **O PIN digitado não conferiu. Peça para digitar de novo.**
     *
     * **Dívida 29.** O par deste código é o [`QuallStatus::NeedsPin`], e os dois conselhos são
     * **opostos**:
     *
     * - `WRONG_PIN` — existe um PIN válido no outro aparelho e a digitação errou. *"PIN errado,
     *   tente de novo"*. Reconectar é obrigatório: vale **uma tentativa por conexão**, e é isso
     *   que segura o PIN de seis dígitos.
     * - `NEEDS_PIN` — o outro aparelho **não reconhece** este pareamento. Digitar o mesmo PIN de
     *   novo não leva a lugar nenhum. *"Peça um PIN novo no outro aparelho"*.
     *
     * Antes desta rodada os dois chegavam como `QUALL_STATUS_PAIRING`, e a casca não tinha como
     * separá-los sem comparar texto de mensagem de erro — que a frente do Windows recusou fazer,
     * e fez bem.
     *
     * Entrou **no fim**, com o valor 15, pela regra de sempre: uma casca compilada contra o
     * header antigo nunca recebia 15, então nenhum valor que ela conhece muda de significado.
     * Ela passa a ver `PAIRING` só no caso residual — o que é uma melhora, não uma quebra.
     */
    QUALL_STATUS_WRONG_PIN = 15,
    /**
     * **O outro aparelho está ocupado: tente de novo daqui a pouco.**
     *
     * Um teleprompter com controle já conectado responde isto a um segundo controle, e ao mesmo
     * controle que volta depois de uma queda enquanto o prompter solta a sessão velha. Ver
     * `docs/contrato-teleprompter.md` §2. Entrou no fim, com o próximo valor livre, pela regra de
     * sempre: nenhum valor que uma casca já conhece muda de significado.
     */
    QUALL_STATUS_BUSY = 16,
    /**
     * **Algo está interferindo na conexão com este aparelho: use o PIN** (`docs/v3-no-privado.md`
     * §3.4, A1). Três pernas do código caíram antes do código aparecer, para o mesmo endereço.
     */
    QUALL_STATUS_INTERFERENCE = 17,
    /**
     * **O outro aparelho não reconheceu este pareamento** (§3.5, A2): falha fechado, na sessão salva e
     * num toque sem confirmador. Ofereça "Esquecer este aparelho" (e então o código) ou o PIN. Com
     * confirmador, o toque segue para o código sozinho (`quall_connect_with_code_to`, §8.6).
     */
    QUALL_STATUS_PAIR_NOT_RECOGNIZED = 18,
    /**
     * **Já há um pareamento com este aparelho** (B2): só diante de um par de build anterior, que
     * recusava substituir pelo código ou pelo QR. Ofereça "Esquecer este aparelho" ou o PIN. Desde
     * 13/10 o código e o QR substituem o vínculo que não retomou (§8.4, §8.6).
     */
    QUALL_STATUS_ALREADY_PAIRED = 19,
} QuallStatus;

/**
 * O que uma track carrega. Espelha `TrackKind` do contrato.
 *
 * **Os valores são ABI.** Espécie nova entra no fim, com o próximo número livre; renumerar ou
 * inserir no meio troca o significado dos números em toda casca já compilada, e o compilador de
 * nenhuma delas teria como perceber. `SYSTEM_AUDIO = 3` entrou depois das outras três, e é por
 * isso que ele está no fim e não ao lado de `MICROPHONE`.
 *
 * As duas espécies de áudio não são a mesma coisa: `MICROPHONE` é a fala de quem transmite,
 * `SYSTEM_AUDIO` é o som que o aparelho está tocando (WASAPI loopback, ScreenCaptureKit,
 * AudioPlaybackCapture). Elas usam presets diferentes — mono a 32 kbit/s contra estéreo a 128
 * kbit/s. Ver `docs/audio.md`.
 */
typedef enum QuallTrackKind {
    QUALL_TRACK_KIND_SCREEN = 0,
    QUALL_TRACK_KIND_CAMERA = 1,
    /**
     * A fala de quem transmite. Mono, Opus a 32 kbit/s, com FEC.
     *
     * A canalização do núcleo está pronta; **nenhuma casca captura microfone ainda**.
     */
    QUALL_TRACK_KIND_MICROPHONE = 2,
    /**
     * O som que o aparelho está tocando. Estéreo, Opus a 128 kbit/s, sem FEC.
     */
    QUALL_TRACK_KIND_SYSTEM_AUDIO = 3,
} QuallTrackKind;

/**
 * Como codificar o áudio de uma track, atravessando a fronteira C.
 *
 * # Por que este campo existe, e o defeito que ele evita
 *
 * `TrackConfig::com_codec_de_audio` existe em Rust desde a rodada do áudio e **não atravessava
 * a fronteira**. A consequência era muda e cara: uma casca C, Swift ou Kotlin que declarasse
 * `QUALL_TRACK_KIND_MICROPHONE` recebia do núcleo um SDP anunciando `opus/48000/2` — porque é o
 * que o preset da espécie diz — e não tinha como dizer que ia mandar µ-law. O fio prometeria
 * Opus a 48 kHz e receberia G.711 a 8 kHz; o outro lado decodificaria com o relógio numa escala
 * 6× errada, **sem erro em lugar nenhum no caminho**.
 *
 * É a mesma classe de defeito do `useinbandfec=1` que não produzia LBRR nenhum, e do SPS sem
 * `bitstream_restriction` do M4: **declarar no fio o que não se faz**. Não dá para consertá-la
 * num lugar e deixar a fronteira obrigando quatro cascas a repeti-la.
 */
typedef enum QuallAudioCodec {
    /**
     * **Use o codec do preset da espécie.** É o valor 0 de propósito.
     *
     * Uma casca que faça `memset(&desc, 0, sizeof desc)` — e uma casca compilada contra o header
     * anterior, que não tinha este campo — cai aqui e obtém exatamente o comportamento de
     * antes. Ver a nota de ABI em [`QuallTrackDesc`].
     */
    QUALL_AUDIO_CODEC_DEFAULT = 0,
    /**
     * Opus a 48 kHz (RFC 7587). O codec do produto.
     */
    QUALL_AUDIO_CODEC_OPUS = 1,
    /**
     * G.711 µ-law a 8 kHz (RFC 3551). O piso: uma tabela de consulta de 8 bits, que qualquer
     * plataforma produz sem biblioteca nenhuma.
     */
    QUALL_AUDIO_CODEC_PCMU = 2,
} QuallAudioCodec;

/**
 * O que aconteceu com a sessão depois que ela subiu. Ver [`quall_session_next_event`].
 */
typedef enum QuallSessionEvent {
    /**
     * Nada por enquanto. Estado normal, não erro.
     */
    QUALL_SESSION_EVENT_NONE = 0,
    /**
     * A outra ponta saiu.
     */
    QUALL_SESSION_EVENT_DISCONNECTED = 1,
    /**
     * O transporte falhou.
     */
    QUALL_SESSION_EVENT_FAILED = 2,
} QuallSessionEvent;

/**
 * Por que o controlador de taxa mexeu (ou não). Espelha `quall_core::taxa::Motivo`.
 */
typedef enum QuallRateReason {
    /**
     * Janela ignorada: carência depois de uma mudança, ou pacotes de menos.
     */
    QUALL_RATE_REASON_SKIPPED = 0,
    /**
     * Desceu. `out_bps` foi escrito.
     */
    QUALL_RATE_REASON_DOWN = 1,
    /**
     * Queria descer e já está no piso. O enlace não dá para este vídeo — ver
     * [`quall_rate_at_floor`].
     */
    QUALL_RATE_REASON_FLOOR = 2,
    /**
     * Subiu. `out_bps` foi escrito.
     */
    QUALL_RATE_REASON_UP = 3,
    /**
     * Queria subir e já está no teto. **É o estado permanente num enlace limpo.**
     */
    QUALL_RATE_REASON_CEILING = 4,
    /**
     * Banda morta, ou ainda contando janelas calmas. Nada a fazer.
     */
    QUALL_RATE_REASON_HOLD = 5,
} QuallRateReason;

/**
 * O que fazer com este slot de 20 ms. Ver [`quall_track_on_audio`].
 *
 * **Um DAC não aceita "pulei este aqui"**: ele vai consumir 20 ms de alguma coisa, e a única
 * escolha é *de qual coisa*. É por isso que não existe uma quarta variante.
 *
 * **Os valores são ABI**, pela mesma regra de [`QuallTrackKind`]: ordem nova entra no fim.
 */
typedef enum QuallAudioOrder {
    /**
     * O pacote chegou. Decodifique normalmente.
     */
    QUALL_AUDIO_ORDER_FRAME = 0,
    /**
     * **O slot não chegou, e o sucessor imediato está em mãos.**
     *
     * `payload` é o pacote *N+1* **inteiro, sem tocar**; `sequence` é o slot que faltou (*N*, e
     * não *N+1*). O convite é chamar `opus_decode(..., decode_fec = 1)` sobre ele.
     *
     * **Confira `fec_has_lbrr` antes.** Sem LBRR, `opus_decode` com `decode_fec = 1` cai na
     * ocultação de perda **em silêncio** e devolve sucesso — quem não conferir vai contar como
     * "curado por FEC" um quadro que o decoder inventou.
     */
    QUALL_AUDIO_ORDER_FEC = 1,
    /**
     * Nem pacote nem socorro. Chame a ocultação de perda (PLC) do decoder.
     */
    QUALL_AUDIO_ORDER_SILENCE = 2,
    /**
     * **Não há fluxo tocando**: escreva **zeros**, e não ocultação de perda. Só sai da porta
     * puxada ([`quall_audio_playout_pull`]): antes da primeira ancoragem, e depois de 10
     * puxadas seguidas sem pacote utilizável. Chega com `payload` nulo, `len` 0,
     * `sequence` 0, `timestamp_us` 0 e `fec_has_lbrr` 0.
     *
     * Entrou em 18/09/2026, **no fim**, pela regra de ABI do enum. A porta empurrada
     * ([`quall_track_on_audio`]) nunca o entrega.
     */
    QUALL_AUDIO_ORDER_IDLE = 3,
} QuallAudioOrder;

/**
 * Os bits de `changed` em [`quall_teleprompter_pump`] e [`quall_teleprompter_peer_lost`]: o que
 * mudou **por causa do outro lado**. Um `uint32_t` com vários bits ligados de uma vez.
 *
 * **Os valores são ABI**: bit novo entra no fim, com o próximo livre.
 */
typedef enum QuallTeleprompterChange {
    QUALL_TELEPROMPTER_CHANGE_TEXT = 1,
    QUALL_TELEPROMPTER_CHANGE_SCROLLING = 2,
    QUALL_TELEPROMPTER_CHANGE_SPEED = 4,
    QUALL_TELEPROMPTER_CHANGE_FONT_SIZE = 8,
    QUALL_TELEPROMPTER_CHANGE_MARGIN = 16,
    QUALL_TELEPROMPTER_CHANGE_READING_LINE = 32,
    QUALL_TELEPROMPTER_CHANGE_MIRROR = 64,
    /**
     * O relato de posição do prompter. **Quem mostra o texto ignora**; o controle desenha.
     */
    QUALL_TELEPROMPTER_CHANGE_POSITION = 128,
    /**
     * Chegou um salto novo. **Quem mostra o texto** vai até `"salto"` do estado, chama
     * [`quall_teleprompter_set_position`] com ele e mantém `rolando` como está. O controle só
     * atualiza a vista.
     */
    QUALL_TELEPROMPTER_CHANGE_JUMP = 256,
    /**
     * Mudou o contato com o outro lado: sumiu, voltou, ou a confirmação das edições daqui mudou.
     * Releia `par_visto_ha_ms` e `sem_confirmacao_ha_ms` no estado.
     */
    QUALL_TELEPROMPTER_CHANGE_PEER = 512,
    /**
     * Mudou `"pergunta_do_texto"` no estado: ela abriu, entrou em "comparando", o texto do
     * prompter nela mudou, ou fechou por causa do outro lado ou de uma sessão nova. Releia o
     * estado (`docs/contrato-teleprompter.md` §11.4).
     */
    QUALL_TELEPROMPTER_CHANGE_TEXT_QUESTION = 1024,
    /**
     * Há cópia nova do roteiro (ou a lista de cópias mudou): **grave o salvo agora**
     * ([`quall_teleprompter_saved_json`]). Acende na bombeada seguinte a qualquer cópia nova —
     * inclusive a que [`quall_teleprompter_resolve_text`] fez (§11.5).
     */
    QUALL_TELEPROMPTER_CHANGE_TEXT_COPY = 2048,
    /**
     * Mudou `"para_tras"` ou `"segurando"` (o "segurar para rolar", §12). **Quem mostra o texto**
     * relê `"rolando"` e `"para_tras"`: com os dois, rola para trás na velocidade de sempre e para
     * no começo. O controle relê `"segurando"`.
     */
    QUALL_TELEPROMPTER_CHANGE_HOLD = 4096,
    /**
     * A gravação (`docs/contrato-teleprompter.md` §13). **No prompter**: chegou um pedido do
     * controle — releia `"pedido_de_gravacao"` e chame [`quall_teleprompter_set_recording`] com o
     * `"gravar"` dele ou [`quall_teleprompter_refuse_recording`] com o `"n"` dele. **No controle**: a gravação começou
     * ou parou (`"gravando_ha_ms"`), o pedido daqui foi respondido (`"pedido_de_gravacao"` voltou a
     * `null`; `"gravacao_recusada"` diz se foi recusado), ou `"par_entende_gravar"` mudou.
     */
    QUALL_TELEPROMPTER_CHANGE_RECORDING = 8192,
    /**
     * **No controle**: `"par_entende_camera"` mudou — o prompter passou a dizer, ou deixou de dizer,
     * que tem a câmera para controlar (`docs/controle-remoto-da-camera.md` §17.3). Com ele, bombeie
     * o `QuallCameraRemote` sobre [`quall_messages_for_camera`]; sem ele, pare.
     */
    QUALL_TELEPROMPTER_CHANGE_CAMERA = 16384,
} QuallTeleprompterChange;

/**
 * **O que mudou no filmador**, em bits, no `changed` de [`quall_camera_host_pump`].
 *
 * **Os valores são ABI**: bit novo entra no fim, com o próximo livre.
 */
typedef enum QuallCameraHostChange {
    /**
     * Há pedido aceito: chame [`quall_camera_host_next_request`] até a fila esvaziar. **Um
     * consumidor só** (a fila do dono da câmera): com várias sessões, várias bombeadas acendem
     * este bit, e só uma thread tira os pedidos.
     */
    QUALL_CAMERA_HOST_CHANGE_REQUEST = 1,
    /**
     * Um receptor disse `ola` ou saiu: releia `"receptores"` no estado.
     */
    QUALL_CAMERA_HOST_CHANGE_LISTENERS = 2,
} QuallCameraHostChange;

/**
 * **O que mudou no receptor por causa do filmador**, em bits, no `changed` de
 * [`quall_camera_remote_pump`].
 *
 * **Os valores são ABI**: bit novo entra no fim, com o próximo livre.
 */
typedef enum QuallCameraRemoteChange {
    /**
     * Chegaram capacidades novas: refaça o painel a partir de `"capacidades"`.
     */
    QUALL_CAMERA_REMOTE_CHANGE_CAPABILITIES = 1,
    /**
     * O ajuste aplicado, o `autor`, a permissão, ou o pendente que caiu.
     */
    QUALL_CAMERA_REMOTE_CHANGE_SETTINGS = 2,
    /**
     * O lido (a linha do R9 §3.6).
     */
    QUALL_CAMERA_REMOTE_CHANGE_READ = 4,
    /**
     * Uma recusa (ou a desistência, `sem_resposta`): mostre `"recusa"` por 3 s.
     */
    QUALL_CAMERA_REMOTE_CHANGE_REFUSAL = 8,
    /**
     * A `"situacao"` mudou.
     */
    QUALL_CAMERA_REMOTE_CHANGE_SITUATION = 16,
} QuallCameraRemoteChange;

/**
 * Anunciante mDNS. Enquanto ele existir, o aparelho aparece na LAN.
 */
typedef struct QuallAdvertiser QuallAdvertiser;

/**
 * Um decodificador de Opus configurado pelo preset da track. Ver [`quall_audio_decoder_new`].
 */
typedef struct QuallAudioDecoder QuallAudioDecoder;

/**
 * Um encoder de áudio configurado pelo preset da track. Ver [`quall_audio_encoder_new`].
 */
typedef struct QuallAudioEncoder QuallAudioEncoder;

/**
 * **A reprodução puxada de uma track de áudio.** Ver `docs/contrato-som-puxado.md`, que fixa os
 * nomes, e `docs/som-no-receptor.md` §3, que diz o porquê.
 *
 * O consumidor fica atrás de um `Mutex` que só `pull` e `free` tocam — e que por contrato uma
 * thread de cada vez —, então ele nunca espera. As leituras (`rate`, `stats_json`) vão pelo
 * leitor, que é outro objeto, e podem vir de qualquer thread sem encostar no consumidor.
 */
typedef struct QuallAudioPlayout QuallAudioPlayout;

/**
 * Navegador: a busca por três caminhos do núcleo (`quall_core::busca`), com a lista que ela mantém.
 *
 * # Desde 12/10, três caminhos e uma thread do núcleo
 *
 * Era só mDNS, e a lista era montada aqui, chamada a chamada. Agora `quall_core::busca::Busca`
 * roda mDNS, broadcast UDP e sondagem da /24 numa thread própria, e mantém a tabela por
 * `DeviceId` com "visto há X s" (`docs/conexao.md` §2). As funções de antes continuam com o mesmo
 * contrato: [`quall_browser_collect`] ainda bloqueia `ms` e devolve quantos há, e o JSON de
 * [`quall_browser_devices_json`] tem as chaves de antes — mais algumas.
 *
 * A tabela é da busca, atrás de um cadeado dela: ler de qualquer thread é seguro (o achado da
 * auditoria que pôs o `Mutex` aqui continua valendo, um andar abaixo).
 */
typedef struct QuallBrowser QuallBrowser;

/**
 * **O filmador** do controle remoto da câmera: um por câmera em uso, compartilhado por todas as
 * sessões de vídeo que a transmitem. Ver [`quall_camera_host_new`].
 */
typedef struct QuallCameraHost QuallCameraHost;

/**
 * **O controle da câmera do outro lado**, no receptor: um por sessão de recepção. Ver
 * [`quall_camera_remote_new`].
 */
typedef struct QuallCameraRemote QuallCameraRemote;

/**
 * O botão **Cancelar** da tela de espera, do lado do núcleo.
 *
 * [`quall_host`] e [`quall_connect`] bloqueiam. Sem isto, a única forma de destravá-los era o
 * contorno que Android e iOS escreveram cada um por sua conta: **abrir uma conexão TCP
 * descartável para o próprio endereço**, só para o `accept` voltar. A frente iOS mediu o
 * contorno: sem cutucada, `quall_host` segura 120,16 s; com cutucada aos 3 s, sai em 3,09 s.
 *
 * # Como usar
 *
 * Crie o cancelador **antes** de largar a thread de trabalho, guarde-o na tela, e passe-o a
 * [`quall_host_cancelable`] ou [`quall_connect_cancelable`]. O botão Cancelar chama
 * [`quall_session_cancel`] de qualquer thread; a chamada bloqueante volta com
 * `QUALL_STATUS_CANCELLED` em [`quall_last_error`] e ponteiro nulo.
 *
 * Cancelar é **irreversível**: para uma segunda tentativa, crie outro cancelador.
 */
typedef struct QuallCanceller QuallCanceller;

/**
 * **A tela "os dois aparelhos mostram o mesmo código?"**, do lado do núcleo.
 *
 * Crie antes de largar a thread de trabalho, passe a [`quall_host_with_code`] ou
 * [`quall_connect_with_code`], e da tela leia [`quall_code_confirm_state_json`] (a cada 100–250
 * ms) e chame [`quall_code_confirm_accept`] / [`quall_code_confirm_reject`] quando a pessoa tocar.
 * Libere com [`quall_code_confirm_free`] depois que a chamada bloqueante voltar.
 *
 * Um por espera: o anfitrião conta nele as tentativas (no máximo 3 por espera).
 */
typedef struct QuallCodeConfirm QuallCodeConfirm;

/**
 * **As mensagens de uma sessão.** Ver [`quall_session_messages`].
 */
typedef struct QuallMessages QuallMessages;

/**
 * **O convite do desktop**: o segredo de 128 bits, a escuta do chamado e o texto do QR.
 *
 * Crie com [`quall_qr_invite_new`], mostre [`quall_qr_invite_text`] como QR, leia
 * [`quall_qr_invite_state_json`] da tela (a cada 250–500 ms) e passe o convite a
 * [`quall_connect_with_invite`] numa thread de trabalho. Libere com [`quall_qr_invite_free`] depois
 * que a chamada bloqueante voltar. Vale 3 min (ou o prazo dado) e **um** chamado: depois disso,
 * crie outro (QR novo).
 */
typedef struct QuallQrInvite QuallQrInvite;

/**
 * O controlador de taxa. Ver `quall_core::taxa`.
 */
typedef struct QuallRate QuallRate;

/**
 * Uma sessão de pé.
 */
typedef struct QuallSession QuallSession;

/**
 * **Uma réplica do estado do teleprompter.** Ver [`quall_teleprompter_new`].
 */
typedef struct QuallTeleprompter QuallTeleprompter;

/**
 * Handle de track entregue à casca. É dono de uma referência, não do recurso.
 */
typedef struct QuallTrack QuallTrack;

/**
 * **Só o anúncio UDP**, sem mDNS (`docs/conexao.md` §2): responde perguntas na UDP 7875 e se
 * anuncia por broadcast. É para a casca que anuncia o mDNS pelo sistema — o iOS, com `NetService`,
 * porque o mDNS do núcleo é socket multicast cru e depende do entitlement de multicast. As
 * respostas unicast (as da sondagem da /24) não dependem dele; o broadcast, sim, e falha calado.
 */
typedef struct QuallUdpResponder QuallUdpResponder;

/**
 * Quem é este aparelho, para o outro lado.
 */
typedef struct QuallDeviceDesc {
    /**
     * Identificador estável, gerado na primeira execução e persistido pela casca. É o que o
     * pareamento vincula — não o nome, que o usuário pode trocar.
     */
    const char *device_id;
    /**
     * Nome real exibido ao par depois da autenticação, sem anunciar em mDNS.
     */
    const char *display_name;
    bool screen_source;
    bool camera_source;
    bool sink;
} QuallDeviceDesc;

/**
 * Uma track de saída a declarar na oferta.
 *
 * # Nota de ABI
 *
 * `audio_codec` entrou **no fim** do struct, e o valor 0 ([`QuallAudioCodec::Default`]) é o
 * comportamento de antes. Os deslocamentos de `kind` e `label` não mudaram, então uma casca
 * compilada contra o header anterior continua escrevendo nos lugares certos — mas ela aloca um
 * struct **menor**, e o núcleo leria além do fim dele. Recompile a casca contra o header novo.
 *
 * A ordem do enum [`QuallTrackKind`] continua sendo ABI pelo motivo registrado em
 * `docs/audio.md` §1, e não foi tocada.
 */
typedef struct QuallTrackDesc {
    enum QuallTrackKind kind;
    /**
     * Rótulo legível mostrado ao usuário no receptor. Ex.: "Tela do Galaxy A10s".
     */
    const char *label;
    /**
     * Só vale em track de áudio; ignorado em vídeo, como o preset já era.
     */
    enum QuallAudioCodec audio_codec;
} QuallTrackDesc;

/**
 * Tudo o que uma sessão precisa saber para subir.
 *
 * # Nota de ABI
 *
 * `bind_address` entrou **no fim** do struct, pela mesma regra que trouxe `audio_codec` em
 * [`QuallTrackDesc`]: `NULL` é o comportamento de antes. Uma casca que faça
 * `memset(&op, 0, sizeof op)` — e uma casca compilada contra o header anterior, que não tinha
 * este campo — cai no `NULL` e obtém exatamente a sessão de antes. Os deslocamentos dos campos
 * que já existiam não mudaram, mas a casca velha aloca um struct **menor** e o núcleo leria
 * além do fim dele: recompile a casca contra o header novo.
 */
typedef struct QuallSessionOptions {
    struct QuallDeviceDesc me;
    /**
     * PIN de seis dígitos. Obrigatório no primeiro pareamento; pode ser nulo quando o par já é
     * conhecido.
     */
    const char *pin;
    /**
     * Estado de pareamento persistido pela casca, como JSON. Nulo começa vazio.
     */
    const char *known_peers_json;
    /**
     * Porta de sinalização. Só vale em [`quall_host`]; `0` deixa o sistema escolher.
     */
    uint16_t signaling_port;
    /**
     * Prazo total: aceitar, parear e o transporte subir.
     */
    uint32_t timeout_ms;
    /**
     * Tracks que **este** aparelho vai emitir. Só vale em [`quall_host`].
     *
     * Elas entram na oferta SDP, e o que não está na oferta só entra com renegociação — que o
     * Quall não implementa. Um emissor que ainda não sabe se vai mandar câmera declara a track
     * mesmo assim e a deixa muda: uma track sem quadro custa uma linha `m=` e nada mais.
     */
    const struct QuallTrackDesc *tracks;
    uintptr_t track_count;
    /**
     * **Prende a mídia a uma interface, e desiste de todas as outras.** `NULL` = o de hoje:
     * o ICE reúne toda interface que a libjuice aceita e escolhe entre elas.
     *
     * O endereço IPv4 **local** — o desta máquina, não o do par — a que os sockets da mídia se
     * prendem. Ex.: `"169.254.75.173"`.
     *
     * # Ligar isto não é "preferir" uma interface: é abrir mão das outras
     *
     * Com o socket preso a um endereço específico, `udp_get_addrs`
     * (`libjuice/src/udp.c:432`) devolve **aquele único** record e retorna antes de qualquer
     * enumeração. Não há candidato de Wi-Fi, não há corrida entre cabo e rádio, e não há
     * recuo automático: se aquela interface não alcançar o par, a sessão não sobe — em vez de
     * subir mais devagar por outro caminho.
     *
     * É exatamente por isso que o campo existe e é exatamente por isso que ele é caro. Prender
     * é o **desvio** do filtro que faz a libjuice recusar `169.254/16`
     * (`libjuice/src/addr.c:84`, e `udp.c:462`: *"we never list link-local addresses"*), e é o
     * único jeito de a mídia entrar num cabo USB-Ethernet. Também é o que obriga o produto a
     * tratar "pelo cabo" como **escolha**, e não como upgrade invisível: uma casca que ligue
     * isto sozinha, sem o usuário ter pedido aquele enlace, troca um caminho que funcionava
     * por um que talvez não funcione.
     *
     * Medido em 2026-09-01, nesta bancada: três iOS emitindo câmera por três cabos ao mesmo
     * tempo, cada sessão presa à sua interface, 1497/1497/1496 quadros e **0,000 % de perda nas
     * três**, com Wi-Fi ligado nos aparelhos. E só o lado que **conecta** precisa prender-se:
     * os três iOS ofereceram apenas candidato de Wi-Fi e o par se formou por *peer-reflexive*.
     * Ver `docs/bancada.md` e `docs/quall-pelo-cabo.md`.
     *
     * # Vazio é `NULL`, e não erro
     *
     * `""` é tratado como "não prenda nada". Não é indulgência: numa casca C um campo de texto
     * que o usuário não preencheu **é** a string vazia — `obs_data_get_string` devolve `""`,
     * nunca `NULL` — e fazer a sessão falhar por isso seria transformar "o campo está em
     * branco" em "a sessão não sobe e não diz por quê". `TransportConfig::bind_address` do
     * núcleo continua recusando `Some("")`, porque lá quem chama é Rust e a distinção entre
     * `None` e `Some("")` existe.
     */
    const char *bind_address;
} QuallSessionOptions;

/**
 * Um quadro já codificado. Espelha `QuadroCodificado` do contrato.
 *
 * Ao **receber**, `annexb` aponta para o buffer de remontagem do núcleo e vale **só durante a
 * chamada** do tratador. Ao **enviar**, o buffer é do chamador e o núcleo não o guarda.
 */
typedef struct QuallFrame {
    /**
     * Um quadro completo em Annex-B, com SPS/PPS junto quando for IDR.
     */
    const uint8_t *annexb;
    uintptr_t len;
    /**
     * Em microssegundos.
     *
     * - **Ao enviar**: o relógio monotônico da captura, **o mesmo de todas as tracks da sessão**
     *   (`docs/contrato-track.md`, "O relógio do `timestamp_us`").
     * - **Ao receber**: desde a **base desta track**, o primeiro quadro entregue. Não é o
     *   relógio do emissor, e duas tracks não se comparam por ele. Para o relógio comum da
     *   sessão, some [`quall_track_capture_offset_us`].
     */
    uint64_t timestamp_us;
    bool idr;
} QuallFrame;

/**
 * Um quadro de áudio já codificado. Espelha `AmostraDeAudio` do contrato.
 *
 * **Não é** o mesmo struct que [`QuallFrame`], e a diferença é deliberada: um quadro de áudio
 * não tem `idr` — todo quadro de Opus é independente — e não tem Annex-B. Fundir os dois
 * obrigaria a um campo que só faz sentido em metade dos usos.
 */
typedef struct QuallAudioSample {
    /**
     * **Um** quadro codificado inteiro: um pacote Opus, ou 20 ms de G.711.
     */
    const uint8_t *payload;
    uintptr_t len;
    /**
     * Relógio monotônico da captura, em microssegundos — **o mesmo do vídeo**, para que as duas
     * tracks da sessão possam ser alinhadas do outro lado.
     */
    uint64_t timestamp_us;
} QuallAudioSample;

/**
 * Um slot de 20 ms saindo do jitter buffer, em ordem de reprodução. Ver [`quall_track_on_audio`].
 *
 * O `payload` aponta para dentro do buffer do núcleo e **vale só durante a chamada**. Copie
 * ali se precisar guardar.
 */
typedef struct QuallAudioSlot {
    enum QuallAudioOrder order;
    /**
     * `FRAME`: o quadro. `FEC`: o pacote *N+1*, o socorro. `SILENCE`: **nulo**, com `len` 0.
     */
    const uint8_t *payload;
    uintptr_t len;
    /**
     * O slot. Em `FEC` é o slot que **faltou**, não o do socorro.
     */
    uint16_t sequence;
    /**
     * Microssegundos desde o primeiro pacote da track, do carimbo RTP. Em `FEC` e `SILENCE` é
     * **interpolado** — não há pacote de onde ler um carimbo. Em `IDLE`, `0`. Para o relógio
     * comum da sessão, some [`quall_track_capture_offset_us`].
     */
    uint64_t timestamp_us;
    /**
     * **O socorro carrega LBRR?** `1` sim, `0` não, **`-1` não sei**.
     *
     * Só é perguntado quando `order == QUALL_AUDIO_ORDER_FEC`; nas outras ordens é `0`.
     *
     * `-1` quer dizer que esta `libquall` foi construída **sem a feature `opus`** e não tem como
     * ler o cabeçalho SILK. Não é "não" — é "não medido", e a distinção é a lição da dívida 26:
     * um contador que finge saber é pior que um que admite não saber. Uma casca que receber `-1`
     * não deve chamar `decode_fec`: ela não sabe se o que voltaria é recuperação ou invenção.
     */
    int8_t fec_has_lbrr;
} QuallAudioSlot;

/**
 * Chamado uma vez por slot de 20 ms, em ordem de reprodução. Ver [`quall_track_on_audio`].
 */
typedef void (*QuallAudioSlotCallback)(const struct QuallAudioSlot *slot, void *user_data);

/**
 * Chamado quando o receptor pede um IDR. Ver [`quall_track_on_idr_request`].
 */
typedef void (*QuallIdrRequestCallback)(void *user_data);

/**
 * Chamado quando um quadro fica pronto. Ver [`quall_track_on_frame`].
 */
typedef void (*QuallFrameCallback)(const struct QuallFrame *frame, void *user_data);

/**
 * O que o teto do núcleo decidiu para uma geometria de captura.
 *
 * Campos em vez de uma string JSON porque isto é consultado **no caminho de abrir a sessão**,
 * uma vez por transmissão, e uma casca que precisasse desserializar JSON para descobrir a
 * largura teria motivo para copiar a aritmética em vez de perguntar — que é exatamente o que
 * esta fronteira existe para evitar.
 */
typedef struct QuallTeto {
    /**
     * Largura a codificar, em pixels. Sempre par, nunca maior que a de entrada.
     */
    uint32_t largura;
    /**
     * Altura a codificar, em pixels. Sempre par, nunca maior que a de entrada.
     */
    uint32_t altura;
    /**
     * Taxa de quadros a pedir à captura **e** ao encoder.
     */
    uint32_t fps;
    /**
     * Macroblocos que o quadro de saída ocupa.
     */
    uint32_t macroblocos;
    /**
     * `MaxFS` do nível anunciado no SDP — o denominador de `macroblocos`.
     */
    uint32_t max_fs;
    /**
     * **Quantos bits por segundo pedir ao encoder para este quadro.**
     *
     * Vem junto com a geometria porque as duas decisões são a mesma decisão. Separá-las produziu
     * o defeito de 01/09/2026: o teto de resolução subiu para 1080p em cinco cascas e o de taxa
     * ficou em 4 000 000 cravado em cada uma — 1080p com 2,25 vezes menos bits por pixel que os
     * 720p que ele substituiu. Ver `quall_core::teto::teto_de_taxa`.
     *
     * 720p30 continua recebendo exatamente 4 000 000: quem não cresceu não muda.
     */
    uint32_t teto_de_taxa_bps;
    /**
     * `level_idc` anunciado no SDP: 31 é o nível 3.1.
     */
    uint8_t level_idc;
    /**
     * A dimensão precisou mudar (1) ou já cabia (0).
     */
    uint8_t reduziu_tamanho;
    /**
     * A taxa de quadros precisou mudar (1) ou já cabia (0).
     */
    uint8_t reduziu_fps;
    /**
     * A saída não é múltipla de 16 nos dois lados, então o SPS **precisa** declarar
     * `frame_cropping` (1). Não é defeito: é o caso comum. Está aqui para o relato da corrida
     * poder dizê-lo — este projeto já quase reprovou uma corrida **por ela estar certa**, quando
     * o iPhone X decodificou em 590x1280 e o roteiro de prova não sabia ler o recorte.
     */
    uint8_t exige_recorte;
} QuallTeto;

/**
 * Chamado quando o Rust entra em pânico. Ver [`quall_install_panic_hook`].
 */
typedef void (*QuallPanicCallback)(const char *message, void *user_data);

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

/**
 * Mensagem do último erro **desta thread**, UTF-8 terminada em NUL.
 *
 * O ponteiro vale até a próxima chamada que falhe nesta mesma thread. Copie se precisar
 * guardar. Nunca é nulo: sem erro, devolve string vazia.
 *
 * **A mensagem é para a pessoa, não para o programa.** Para o programa decidir o que fazer,
 * leia [`quall_last_status`]: o texto está em português e muda quando alguém o reescreve.
 *
 * # Safety
 *
 * O ponteiro não deve ser liberado por quem chama.
 */
const char *quall_last_error(void);

/**
 * **Código da última falha desta thread** — o par de [`quall_last_error`], para o programa.
 *
 * # Por que ele existe
 *
 * [`quall_host`] e [`quall_connect`] devolvem **ponteiro nulo** quando falham, e o motivo só
 * existia como texto. Isso morde um caso nomeado e decidido: `QUALL_STATUS_NEEDS_PIN` não é
 * recusa, é convite a recomeçar — a casca deve mostrar a tela de PIN em vez de "falhou"
 * (dívida 22). Para distingui-lo de "IP errado" ou "o ICE não achou caminho", a casca teria de
 * **comparar prefixo de string em português** — que é exatamente o defeito que fez
 * [`QuallStatus::NoRoute`] existir. O receptor Android desistiu de distinguir por causa disso.
 *
 * Vale para toda função que sinaliza falha sem devolver `QuallStatus`: as que devolvem ponteiro
 * (`quall_host`, `quall_connect`, `quall_browser_start`, `quall_advertiser_start`,
 * `quall_session_track`, `quall_session_next_track`) e as do padrão `(buf, cap)`, que devolvem
 * negativo.
 *
 * # A regra de leitura, e ela é a mesma de `quall_last_error`
 *
 * **Leia logo depois da chamada que falhou, antes de qualquer outra função `quall_`,** e só
 * quando aquela chamada de fato sinalizou falha. Uma chamada bem-sucedida **não** limpa este
 * valor — sem falha nenhuma nesta thread ele é `QUALL_STATUS_OK`, e depois de uma falha ele
 * fica como estava até a próxima. Perguntar "deu erro?" a esta função é ler um valor velho; a
 * pergunta certa é ao valor devolvido pela chamada (nulo, ou negativo).
 *
 * Em C, e **sem comentário dentro do exemplo**: o cbindgen copia este texto para dentro de um
 * bloco de comentário do `quall.h`, e um fecha-comentário escrito aqui fecharia aquele bloco no
 * meio da prosa. O teste `nenhum_doc_comment_do_header_fecha_no_meio` é a tranca.
 *
 * ```c
 * QuallSession *s = quall_connect(ip, &opcoes);
 * if (!s) {
 *     switch (quall_last_status()) {
 *     case QUALL_STATUS_WRONG_PIN: pedir_o_pin_de_novo();      break;
 *     case QUALL_STATUS_NEEDS_PIN: pedir_um_pin_novo_la();     break;
 *     case QUALL_STATUS_NO_ROUTE:  explicar_rede_local();      break;
 *     case QUALL_STATUS_CANCELLED: voltar_sem_dizer_nada();    break;
 *     default:                     mostrar(quall_last_error()); break;
 *     }
 * }
 * ```
 */
enum QuallStatus quall_last_status(void);

/**
 * Começa a anunciar este aparelho por mDNS na porta de sinalização dada.
 *
 * # Safety
 *
 * `me` precisa apontar para um [`QuallDeviceDesc`] válido.
 */
struct QuallAdvertiser *quall_advertiser_start(const struct QuallDeviceDesc *me,
                                               uint16_t signaling_port);

/**
 * Rótulo público efêmero do anunciante (`Quall <prefix8>`), igual ao mostrado na descoberta.
 * Padrão `(buf, cap)`: tamanho UTF-8 incluindo NUL; não escreve se não couber; `-1` em erro.
 * O nome real do aparelho só é enviado depois da autenticação.
 *
 * # Safety
 * `a` precisa vir de `quall_advertiser_start`/`_with_role` e continuar vivo durante a chamada.
 * `buf` precisa ser nulo ou apontar para `cap` bytes graváveis.
 */
intptr_t quall_advertiser_label(const struct QuallAdvertiser *a, char *buf, uintptr_t cap);

/**
 * Para de anunciar e libera. Nulo é ignorado.
 *
 * # Dívida 3: isto não desregistrava nada
 *
 * A versão anterior só soltava a caixa, e o `Advertiser` **não tinha `Drop`** — apesar de o
 * comentário dele afirmar que tinha. Cada sessão deixava para trás uma thread de daemon mDNS
 * viva e um anúncio fantasma na lista dos outros aparelhos até o TTL expirar. No desktop o
 * processo morre e limpa; no Android o processo sobrevive a dezenas de sessões.
 *
 * Agora esta função desregistra e espera a confirmação, como o [`quall_browser_stop`] já fazia.
 * O `Drop` do `Advertiser` faz o mesmo, para quem esquecer de chamar.
 *
 * **Bloqueia** por até ~1 s esperando o adeus sair. Chame da thread de trabalho.
 *
 * # Safety
 *
 * `a` precisa vir de [`quall_advertiser_start`] e não pode ter sido liberado antes.
 */
void quall_advertiser_stop(struct QuallAdvertiser *a);

/**
 * Começa a procurar aparelhos Quall na LAN pelos três caminhos.
 *
 * Não falha por falta de mDNS ou de porta UDP: a busca segue com o que abriu. Nulo só se nem a
 * thread da busca subir.
 */
struct QuallBrowser *quall_browser_start(void);

/**
 * **Os pares conhecidos, para a lista dizer quem é quem** (`docs/v3-no-privado.md` §5.4, P3). No
 * v3 o anúncio não traz o id de ninguém; com isto, cada item novo da lista é sondado uma vez (um
 * `Probe`, e a conexão fecha antes do `Hello`: nada é gasto do outro lado) e, se for um dos pares
 * de `opcoes.known_peers_json`, o item de [`quall_browser_devices_json`] ganha `known_device_id`
 * com o id que a casca guardou — mostre o nome local e conecte com
 * [`quall_connect_with_code_to`] (`expected_device_id`) ou [`quall_connect_known`].
 *
 * Só `opcoes.me` (o id deste aparelho) e `opcoes.known_peers_json` são lidos. `role`: `NULL`/`""`
 * para vídeo, `"controle_remoto"` para o controle. Chame de novo quando a tabela mudar (parear,
 * esquecer): o cache recomeça. `known_peers_json` vazio desliga. As sondas não andam sozinhas:
 * cada [`quall_browser_devices_json`] põe para sondar os itens novos (no máximo 2 de uma vez, 2,5 s
 * cada; quem não respondeu volta em 30 s), e o rótulo aparece numa das chamadas seguintes.
 *
 * Devolve `0`, ou `-1` com [`quall_last_status`] (`INVALID`, `NULL_ARGUMENT`).
 *
 * # Safety
 *
 * `b` precisa vir de [`quall_browser_start`]; `opcoes` válido durante a chamada; `role` nulo ou
 * uma string C válida.
 */
int32_t quall_browser_set_known(const struct QuallBrowser *b,
                                const struct QuallSessionOptions *opcoes,
                                const char *role);

/**
 * **"Procurar de novo"** (`docs/v3-no-privado.md` §8.9): a varredura da /24 recomeça agora, com a
 * lista cheia ou não, no ritmo baixo de sempre (~5 s por /24). Sem isto, a busca só varre a /24
 * sozinha com a lista vazia, uma vez por processo a cada 5 min; o resto do tempo ela pergunta só às
 * dicas ([`quall_browser_add_hint`]) e aos vizinhos da tabela ARP. Ligue ao botão de procurar de
 * novo da tela — não a um temporizador.
 *
 * Devolve `0`, ou `-1` com [`quall_last_status`] (`NULL_ARGUMENT`).
 *
 * # Safety
 *
 * `b` precisa vir de [`quall_browser_start`].
 */
int32_t quall_browser_rescan(const struct QuallBrowser *b);

/**
 * **Uma dica** (`docs/v3-no-privado.md` §8.9): um endereço onde um aparelho conhecido esteve — o
 * último de um vínculo, o da sessão salva, o último do controle —, `"ip"` ou `"ip:porta"` (a porta
 * é ignorada). A busca o pergunta por unicast aos 1 s e mais duas vezes (20 s e 40 s depois), só
 * enquanto ele não está na lista e não foi tocado há pouco. Só IPv4 privado conta; o resto é
 * ignorado sem erro. Pode chamar várias vezes (uma por vínculo).
 *
 * Devolve `0`, ou `-1` com [`quall_last_status`] (`NULL_ARGUMENT`, `INVALID` para texto que não é
 * endereço).
 *
 * # Safety
 *
 * `b` precisa vir de [`quall_browser_start`]; `endereco` uma string C válida.
 */
int32_t quall_browser_add_hint(const struct QuallBrowser *b,
                               const char *endereco);

/**
 * Espera `ms` milissegundos e devolve quantos aparelhos há **no total** na lista. Negativo em erro.
 *
 * A busca anda sozinha numa thread do núcleo; esta função só dá a cadência de quem desenha a
 * tela. Chamar em laço com `ms` curto (250–400) é o uso esperado.
 *
 * # Safety
 *
 * `b` precisa vir de [`quall_browser_start`].
 */
int32_t quall_browser_collect(const struct QuallBrowser *b,
                              uint32_t ms);

/**
 * Escreve os aparelhos achados como **JSON**, no padrão `(buf, cap)` do topo do módulo.
 *
 * Um array de objetos com as chaves de sempre — `device_id`, `display_name`, `protocol_version`,
 * `capabilities` (`screen_source`, `camera_source`, `sink`), `endpoint` (`"ip:porta"` ou `null`) e
 * `papel` (só quando há) — e, desde 12/10 (`docs/conexao.md` §2.4 e §4):
 *
 * Formato: um array de objetos com `device_id`, `display_name`, `protocol_version`,
 * `capabilities` (`screen_source`, `camera_source`, `sink`) e `endpoint` (`"ip:porta"` ou
 * `null` quando o aparelho não anunciou endereço utilizável).
 * `identity_authenticated` é `false`: o ID e nome desta lista são placeholders efêmeros,
 * não servem para consultar ou persistir pareamentos. Use o peer da sessão após autenticar.
 * - `"alive"`: visto há no máximo 12 s. `false` é "visto há X s": o item fica mais 30 s e sai.
 * - `"seen_ms_ago"`: há quanto tempo veio o último sinal de vida (0 enquanto o mDNS o mantém).
 * - `"via"`: por onde foi visto — `"mdns"`, `"udp"`, `"sondagem"`.
 * - `"accepts_code"`: aceita a confirmação por código de 4 dígitos.
 * - `"own"`: é um anúncio deste mesmo processo (o token é do anunciante daqui). Não liste.
 * - `"known_device_id"`: o id guardado do par que este item é, quando a casca deu os pares em
 *   [`quall_browser_set_known`] e a sonda já reconheceu; `null` nos outros casos. Mostre o nome
 *   local e conecte com o alvo (A2).
 * - `"announced_name"` (13/10, `docs/v3-no-privado.md` §8.7): o nome que o aparelho escolheu para a
 *   rede (`quall_set_announced_name` do lado de lá), saneado — até 40 bytes, sem controle; `null`
 *   quando o anúncio não traz nome (build das lojas, campo apagado). **Não autentica nada.** A linha
 *   mostra, nesta ordem: o nome guardado do par (`known_device_id`) > este nome, com o alias
 *   (`display_name`) pequeno embaixo > só o alias.
 *
 * Vivos primeiro, depois pelo nome anunciado (ou o alias). Uma casca que não conhece as chaves novas segue funcionando.
 *
 * # Safety
 *
 * `b` precisa vir de [`quall_browser_start`]; `buf` precisa ser nulo ou ter `cap` bytes.
 */
intptr_t quall_browser_devices_json(const struct QuallBrowser *b,
                                    char *buf,
                                    uintptr_t cap);

/**
 * **Por que a lista pode estar vazia**, como JSON (`docs/conexao.md` §2.5). Padrão `(buf, cap)`:
 *
 * ```json
 * {"causes":["firewall","isolamento"],"networks":["192.168.15.0/24"],"probes":1,"listening":true}
 * ```
 *
 * `causes`, da mais para a menos provável: `"sem_rede_local"`, `"sub_rede_diferente"`,
 * `"firewall"`, `"isolamento"` — a casca traduz cada uma. `networks` são as redes deste aparelho,
 * para a tela dizer "este aparelho está na rede 192.168.15.x; o outro precisa estar na mesma".
 * Vazio de causas é "ainda procurando" ou "achou".
 *
 * # Safety
 *
 * `b` precisa vir de [`quall_browser_start`]; `buf` precisa ser nulo ou ter `cap` bytes.
 */
intptr_t quall_browser_hint_json(const struct QuallBrowser *b,
                                 char *buf,
                                 uintptr_t cap);

/**
 * Para de procurar e libera. Nulo é ignorado. Bloqueia até a thread da busca sair (uma fatia,
 * ~50 ms, mais o adeus do mDNS).
 *
 * # Safety
 *
 * `b` precisa vir de [`quall_browser_start`] e não pode ter sido liberado antes.
 */
void quall_browser_stop(struct QuallBrowser *b);

/**
 * **Acha o aparelho da sessão salva pelo `device_id`**, quando o DHCP pode ter trocado o IP
 * (`docs/conexao.md` §3). **Bloqueia** até achar ou até `timeout_ms` (0 = 8 s).
 *
 * `last_endpoint` (`"ip:porta"`, ou nulo) é o endereço guardado: ele é tentado por TCP com prazo
 * de 1 s enquanto a busca pergunta pelo id. Padrão `(buf, cap)`:
 *
 * ```json
 * {"endpoint":"192.168.15.9:7877","via":"udp","changed":true,"accepts_code":true}
 * ```
 *
 * `changed` verdadeiro: grave o endereço novo na sessão salva, **sem perguntar**, e conecte nele
 * com `quall_connect*` — sem PIN, porque o par já é conhecido e a retomada fecha sozinha. `via`
 * é `"endereco_antigo"`, `"udp"`, `"sondagem"` ou `"mdns"`. Não achou: `-1` com
 * `QUALL_STATUS_TIMEOUT` e as causas prováveis em [`quall_last_error`].
 *
 * # Safety
 *
 * `device_id` precisa ser uma string C válida; `last_endpoint`, nula ou válida; `buf`, nulo ou
 * com `cap` bytes.
 */
intptr_t quall_locate_peer_json(const char *device_id,
                                const char *last_endpoint,
                                uint32_t timeout_ms,
                                char *buf,
                                uintptr_t cap);

/**
 * **O nome que os outros aparelhos veem na lista** (`docs/v3-no-privado.md` §8.7, a P3 revista):
 * define o nome que **todo anúncio deste processo que começar depois** leva na chave `nm` —
 * [`quall_advertiser_start`] e as variantes, [`quall_udp_responder_start`] e a variante com token.
 * O anúncio já no ar segue com o nome com que começou; a próxima espera leva o novo.
 *
 * O nome é saneado aqui (sem controle nem quebra, sem caracteres invisíveis de formatação, espaços
 * em sequência viram um, até 40 bytes UTF-8 sem partir caractere). `NULL`, `""` ou um nome que o
 * saneamento esvazia **apaga**: os anúncios saem sem nome e a lista dos outros mostra o alias
 * `Quall <8 hex>`. O id e as chaves nunca vão no anúncio.
 *
 * Chame ao abrir o app (com o nome guardado nos Ajustes, ou o do sistema na primeira vez) e quando
 * a pessoa mudar o campo. É do processo: a extensão de transmissão do iOS chama no próprio
 * processo. Devolve `QUALL_STATUS_OK`, ou `QUALL_STATUS_NOT_UTF8` com `name` que não é UTF-8 (o
 * nome anterior fica).
 *
 * # Safety
 *
 * `name` precisa ser nulo ou apontar para uma string terminada em zero.
 */
enum QuallStatus quall_set_announced_name(const char *name);

/**
 * O nome que o próximo anúncio deste processo vai levar, **já saneado** — o que os outros vão ver
 * —, ou `""` sem nome. Padrão `(buf, cap)`. Serve à casca que publica o mDNS por fora do núcleo (o
 * iOS, no TXT do `mDNSResponder`, chave `nm`) e aos Ajustes, para mostrar como o nome ficou.
 *
 * # Safety
 *
 * `buf` precisa ser nulo ou apontar para `cap` bytes graváveis.
 */
intptr_t quall_announced_name(char *buf,
                              uintptr_t cap);

/**
 * Começa a anunciar pela UDP. `role` e `accepts_code` como em [`quall_advertiser_start_with_code`].
 * Libere com [`quall_udp_responder_stop`]. Nulo só em parâmetro inválido.
 *
 * # Safety
 *
 * As mesmas de [`quall_advertiser_start_with_code`].
 */
struct QuallUdpResponder *quall_udp_responder_start(const struct QuallDeviceDesc *me,
                                                    uint16_t signaling_port,
                                                    const char *role,
                                                    bool accepts_code);

/**
 * [`quall_udp_responder_start`] com o **token** do anúncio que a casca publica por fora do núcleo
 * (o iOS, pelo `mDNSResponder`): os dois anúncios dizem o mesmo token, e o aparelho é **um** item
 * na lista dos outros (`docs/v3-no-privado.md` §5.3). `token`: 32 hex minúsculos; outro formato é
 * `INVALID`.
 *
 * # Safety
 *
 * As de [`quall_udp_responder_start`]; `token` precisa apontar para uma string terminada em zero.
 */
struct QuallUdpResponder *quall_udp_responder_start_with_token(const struct QuallDeviceDesc *me,
                                                               uint16_t signaling_port,
                                                               const char *role,
                                                               bool accepts_code,
                                                               const char *token);

/**
 * Para de anunciar pela UDP e libera. Nulo é ignorado. Bloqueia até a thread do respondedor sair
 * quando este era o último anúncio do processo (~50 ms).
 *
 * # Safety
 *
 * `r` precisa vir de [`quall_udp_responder_start`] e não pode ter sido liberado antes.
 */
void quall_udp_responder_stop(struct QuallUdpResponder *r);

struct QuallCodeConfirm *quall_code_confirm_new(void);

/**
 * O estado para a tela, como JSON. Padrão `(buf, cap)`:
 *
 * ```json
 * {"phase":"mostrando","code":"0427","peer":null,"attempts":1,"choices":[],"wrong_choice":false,"code_blocked":false}
 * {"phase":"mostrando","code":null,"peer":"Galaxy A07","attempts":1,"choices":["8812","0427","5530"],"wrong_choice":false,"code_blocked":false}
 * ```
 *
 * O primeiro é o **convidado**: mostre `code` (e um Recusar). O segundo é o **anfitrião**
 * (`docs/v3-no-privado.md` §3.6): `code` é sempre `null` — mostre os três `choices` como botões
 * iguais, o nome de quem pede (`peer`) e "Nenhum destes"; o toque vai a
 * [`quall_code_confirm_choose`]. O nome do anfitrião só chega ao convidado em `"conferiu"` (M2).
 *
 * `phase`: `"esperando"` (nada a mostrar), `"mostrando"`, `"confirmado_aqui"` (esperando o outro
 * lado), `"conferiu"`, `"recusado_aqui"`, `"recusado_la"`, `"nao_conferiu"` (alguém no meio, ou
 * erro), `"expirou"`. `wrong_choice`: a escolha foi errada; `code_blocked`: o código desta espera
 * acabou (P11), só o PIN serve. `code` e `peer` são `null` quando não há. `refusal`: por que o
 * núcleo recusou a última troca, quando há o que dizer — `"already_paired"` (já há pareamento com
 * esse aparelho aqui, B2: ofereça "Esquecer este aparelho" ou o PIN), nos dois lados;
 * `"code_not_accepted"` (convidado: o outro aparelho fechou logo depois do pedido do código — ele
 * não aceita o código; a chamada volta com `NEEDS_PIN` e o cartão fica em `"esperando"`: peça o PIN
 * da tela de lá, nunca "recusou"); `null` nos outros desfechos. `"recusado_la"` só vem depois de um
 * código na tela. `notice`: `"previous_pairing_invalid"` no convidado cujo toque tentou reconectar e
 * o outro lado não reconheceu — o código é para parear de novo ([`quall_connect_with_code_to`]); `null`
 * nos outros casos.
 *
 * # Safety
 *
 * `c` precisa vir de [`quall_code_confirm_new`]; `buf`, nulo ou com `cap` bytes.
 */
intptr_t quall_code_confirm_state_json(const struct QuallCodeConfirm *c,
                                       char *buf,
                                       uintptr_t cap);

/**
 * **Anfitrião**: a pessoa tocou o número `index` de `"choices"` (`docs/v3-no-privado.md` §3.6) no
 * cartão da tentativa `attempt`. O número que o outro aparelho mostra confirma; um falso recusa e
 * encerra o código desta espera (`"code_blocked": true`; o PIN continua). Fora da tentativa ou da
 * fase, ou no convidado, devolve `false` e não faz nada. No anfitrião,
 * [`quall_code_confirm_accept`] não confirma mais: é preciso escolher.
 *
 * # Safety
 *
 * `c` precisa vir de [`quall_code_confirm_new`] e estar vivo.
 */
bool quall_code_confirm_choose(const struct QuallCodeConfirm *c,
                               uint32_t attempt,
                               uint32_t index);

/**
 * A pessoa tocou **Confirmar** no cartão da tentativa `attempt` — o `"attempts"` do
 * [`quall_code_confirm_state_json`] que a tela estava mostrando. Vale só se **essa** tentativa ainda
 * está com o código na tela: um toque dado olhando o cartão de uma tentativa que já caiu **não**
 * confirma a seguinte (revisão adversarial de 12/10, M2). `true` se o toque valeu. Nulo é ignorado
 * (`false`). Qualquer thread.
 *
 * # Safety
 *
 * `c` precisa ser nulo ou vir de [`quall_code_confirm_new`].
 */
bool quall_code_confirm_accept(const struct QuallCodeConfirm *c,
                               uint32_t attempt);

/**
 * A pessoa tocou **Recusar**. Mesmas regras de [`quall_code_confirm_accept`].
 *
 * # Safety
 *
 * `c` precisa ser nulo ou vir de [`quall_code_confirm_new`].
 */
bool quall_code_confirm_reject(const struct QuallCodeConfirm *c, uint32_t attempt);

/**
 * Libera. Nulo é ignorado. Pode ser liberado depois que a chamada bloqueante voltar: ela segura a
 * própria cópia do estado.
 *
 * # Safety
 *
 * `c` precisa vir de [`quall_code_confirm_new`] e não pode ter sido liberado antes.
 */
void quall_code_confirm_free(struct QuallCodeConfirm *c);

/**
 * **Anuncia dizendo se aceita o código.** `role` como em [`quall_advertiser_start_with_role`]
 * (`NULL`, `""` ou `"teleprompter"`); `accepts_code` só pode ser `true` se a casca vai hospedar com
 * [`quall_host_with_code`] e tem a tela de confirmação. Com `false` e `role` nulo, é exatamente
 * [`quall_advertiser_start`].
 *
 * # Safety
 *
 * As mesmas de [`quall_advertiser_start_with_role`].
 */
struct QuallAdvertiser *quall_advertiser_start_with_code(const struct QuallDeviceDesc *me,
                                                         uint16_t signaling_port,
                                                         const char *role,
                                                         bool accepts_code);

/**
 * Cria o convite: sorteia o segredo e abre a escuta TCP do chamado (porta 7874, ou efêmera se
 * ocupada), com os endereços das redes locais deste aparelho. `ttl_ms` 0 = 180 000.
 *
 * Nulo em erro, com o motivo em [`quall_last_status`]: `NO_ROUTE` quando o aparelho não está em
 * rede local nenhuma (avião, só dados móveis) — a tela diz isso em vez de mostrar um QR inútil.
 *
 * # Safety
 *
 * `me` precisa apontar para um [`QuallDeviceDesc`] válido (`device_id` e `display_name`).
 */
struct QuallQrInvite *quall_qr_invite_new(const struct QuallDeviceDesc *me,
                                          uint32_t ttl_ms);

/**
 * O texto do QR, `quall://chamar?v=2&a=…&p=…&i=…&n=…&s=…`. Padrão `(buf, cap)`. **Leva o
 * segredo**: vai para o desenho do QR e para mais nada — nem diário, nem área de transferência.
 *
 * # Safety
 *
 * `c` precisa vir de [`quall_qr_invite_new`]; `buf`, nulo ou com `cap` bytes.
 */
intptr_t quall_qr_invite_text(const struct QuallQrInvite *c,
                              char *buf,
                              uintptr_t cap);

/**
 * O estado para a tela, como JSON. Padrão `(buf, cap)`:
 *
 * ```json
 * {"phase":"esperando","expires_in_ms":171234,"addresses":["192.168.15.9"],"port":7874,
 *  "peer_id":null,"peer_name":null,"endpoint":null}
 * ```
 *
 * `phase`: `"esperando"` (mostre o QR e quanto falta), `"chamado"` (tire o QR e mostre
 * "Conectando a `peer_name`…"; `peer_id` e `endpoint` preenchidos) ou `"expirou"` (troque por um
 * convite novo). `addresses` e `port` são para o diário e para a dica "este computador está na
 * rede 192.168.15.x".
 *
 * # Safety
 *
 * `c` precisa vir de [`quall_qr_invite_new`]; `buf`, nulo ou com `cap` bytes.
 */
intptr_t quall_qr_invite_state_json(const struct QuallQrInvite *c, char *buf, uintptr_t cap);

/**
 * **O desktop recebe pelo QR.** Bloqueia até um celular chamar (ou o convite expirar, ou o
 * cancelador disparar), e então conecta nele como receptor e pareia pela chave do QR — sem PIN.
 *
 * `opcoes` como em [`quall_connect`] (`pin` é ignorado; `known_peers_json` é lido e o pareamento
 * novo sai em [`quall_session_known_peers_json`] para a casca guardar, como sempre). Depois disso o
 * par é conhecido e as próximas vezes são retomada, por `quall_connect*`.
 *
 * Nulo em erro, com [`quall_last_status`]: `TIMEOUT` (o QR expirou sem chamado: mostre um novo),
 * `CANCELLED`, `PAIRING` (quem atendeu não é quem leu o QR), e os de sempre de `quall_connect`.
 * Um convite já usado numa chamada anterior devolve o mesmo chamado — crie outro para tentar de
 * novo.
 *
 * # Safety
 *
 * `c` precisa vir de [`quall_qr_invite_new`] e continuar vivo durante a chamada; `opcoes`, válido
 * durante a chamada; `cancelador`, nulo ou de [`quall_canceller_new`].
 */
struct QuallSession *quall_connect_with_invite(const struct QuallQrInvite *c,
                                               const struct QuallSessionOptions *opcoes,
                                               const struct QuallCanceller *cancelador);

/**
 * [`quall_connect_with_invite`] **com um papel**: `role` = `"controle_remoto"` é o controle que mostra
 * o QR para o **teleprompter** ler (a espera dele, na 7979, só aceita controle remoto). `NULL` ou `""`
 * é exatamente [`quall_connect_with_invite`]; outro texto é `INVALID`. Como em
 * [`quall_connect_with_role`], diante de um aparelho que não serve ao papel a saída é `PROTOCOL`, e
 * diante de um prompter que já tem controle, `BUSY`. A porta vem do chamado (o prompter chama com a
 * dele), então não há porta a completar.
 *
 * # Safety
 *
 * As mesmas de [`quall_connect_with_invite`]; `role` precisa ser nulo ou uma string C válida.
 */
struct QuallSession *quall_connect_with_invite_role(const struct QuallQrInvite *c,
                                                    const struct QuallSessionOptions *opcoes,
                                                    const struct QuallCanceller *cancelador,
                                                    const char *role);

/**
 * Libera o convite e fecha a escuta. Nulo é ignorado. Bloqueia até a thread da escuta sair (~50
 * ms).
 *
 * # Safety
 *
 * `c` precisa vir de [`quall_qr_invite_new`], não pode ter sido liberado antes, e nenhuma chamada
 * com ele pode estar em andamento.
 */
void quall_qr_invite_free(struct QuallQrInvite *c);

/**
 * **O celular lê um QR**: diz se é um código de conexão do Quall e de quem, sem rede e sem guardar
 * nada. Para a tela mostrar "Conectar a MacBook do Pessoa?" antes de chamar. Padrão `(buf, cap)`:
 *
 * ```json
 * {"peer_id":"mac-3F2504E0","peer_name":"MacBook do Pessoa","addresses":["192.168.15.9"],"port":7874}
 * ```
 *
 * `-1` com `INVALID` quando não é um QR do Quall, está quebrado ou é de versão mais nova (a
 * mensagem diz "atualize o app"). O segredo não sai daqui.
 *
 * # Safety
 *
 * `text` precisa ser uma string C válida; `buf`, nulo ou com `cap` bytes.
 */
intptr_t quall_qr_read_json(const char *text,
                            char *buf,
                            uintptr_t cap);

/**
 * **O celular chama o desktop do QR.** Chame com a espera já de pé neste processo (`quall_host*`
 * bloqueado numa thread, o PIN na tela) e `signaling_port` sendo a porta dela: o desktop conecta
 * ali logo depois de aceitar, e a espera aceita o pareamento pelo QR sem mudança nenhuma de
 * chamada. Bloqueia até a resposta (`timeout_ms` 0 = 5 000). **É ação, não consulta**: chame
 * **uma vez**, com um buffer de **pelo menos 512 bytes** (a resposta é menor que isso): com `buf`
 * nulo ou `cap` menor, volta `-1` com `INVALID` **sem ir à rede** — perguntar o tamanho antes faria
 * o chamado, e a segunda chamada ouviria "usado". Devolve como o padrão
 * `(buf, cap)`:
 *
 * ```json
 * {"result":"aceito","peer_id":"mac-3F2504E0","peer_name":"MacBook do Pessoa","via":"192.168.15.9:7874"}
 * {"result":"recusado","reason":"expirou"}
 * ```
 *
 * `reason`: `"expirou"` (o QR do computador venceu: peça um novo), `"usado"` (outro aparelho chamou
 * antes com este QR), `"invalido"` (o QR não é deste convite), `"endereco"` (o computador vê este
 * celular por outro endereço: VPN ligada, ou outra rede — diga "Desligue a VPN ou use a mesma
 * rede"). O chamado vai selado (`docs/v3-no-privado.md` §4): nem o id nem o nome deste aparelho
 * passam em claro pela rede. `-1` com `NO_ROUTE` quando ninguém
 * respondeu nos endereços do QR (outra rede, firewall), `PAIRING` quando quem respondeu não provou
 * ser o computador do QR, `INVALID` para QR ilegível ou porta 0.
 *
 * **Ler e hospedar no mesmo processo** (`docs/conexao.md` §10.6): a chave lida fica numa caixa do
 * processo por 2 min e 3 tentativas, e é dali que a espera a tira.
 *
 * # Safety
 *
 * `text` e `me` válidos durante a chamada; `buf`, nulo ou com `cap` bytes.
 */
intptr_t quall_qr_call_json(const char *text,
                            const struct QuallDeviceDesc *me,
                            uint16_t signaling_port,
                            uint32_t timeout_ms,
                            char *buf,
                            uintptr_t cap);

/**
 * **Como terminou o pareamento pelo QR, no celular que o leu** (B2). Depois de
 * [`quall_qr_call_json`] dar `"aceito"`, o desktop conecta na espera deste processo; a espera não
 * volta por um candidato recusado (ela segue esperando), então a tela de "Conectando a X…" lê isto
 * para saber o desfecho. `peer_id` é o `peer_id` do QR. Padrão `(buf, cap)`:
 *
 * ```json
 * {"outcome":"pending"}
 * ```
 *
 * `outcome`: `"pending"` (ainda não terminou), `"connected"` (a sessão subiu: a espera voltou com
 * ela — também quando o QR virou retomada, com o vínculo bom dos dois lados), `"already_paired"`
 * (só diante de um computador de build anterior, que recusava o QR com vínculo: ofereça
 * "Esquecer este aparelho" ou o PIN; desde 13/10 o QR reconecta ou substitui,
 * `docs/v3-no-privado.md` §8.4), `"failed"` (a troca não fechou: peça um QR novo). O
 * desfecho vale 2 min, e cada [`quall_qr_call_json`] novo ao mesmo computador o apaga: a
 * tentativa nova começa em `"pending"`.
 *
 * # Safety
 *
 * `peer_id` precisa ser uma string C válida; `buf`, nulo ou com `cap` bytes.
 */
intptr_t quall_qr_outcome_json(const char *peer_id,
                               char *buf,
                               uintptr_t cap);

/**
 * **Espaça a saída de vídeo das próximas sessões** a no máximo `kbps` kbit/s. `0` desliga, e é o
 * padrão — o de toda casca que não chamar isto.
 *
 * Vale para as sessões abertas **depois** da chamada, e só nas tracks de vídeo; uma sessão já
 * aberta fica como nasceu. É do processo, e não da sessão, para não mexer em
 * [`QuallSessionOptions`], cujo layout as cascas repetem à mão.
 *
 * Medido em 11/09/2026 com o Mac no cabo (`docs/tela-estendida.md`, corrida B): espalhar a saída a
 * 60 Mbit/s levou a perda de um receptor vizinho de 7,4 % para 0,17 %. **Tem de ficar bem acima
 * da taxa do vídeo**: o espaçador enfileira o que passa da verba, e a fila não tem teto. Ver
 * `TrackConfig::espacamento_kbps` no núcleo.
 */
void quall_set_video_pacing_kbps(uint32_t kbps);

/**
 * Cria um cancelador ainda não acionado. Libere com [`quall_canceller_free`].
 */
struct QuallCanceller *quall_canceller_new(void);

/**
 * **Cancela a espera.** Pode ser chamado de qualquer thread, quantas vezes quiser; nulo é
 * ignorado.
 *
 * Este é o `quall_session_cancel` que a dívida 10 pede. Ele recebe o **cancelador**, e não a
 * sessão, porque enquanto se espera ainda não existe sessão nenhuma — é exatamente o buraco que
 * obrigava as cascas ao contorno da conexão descartável.
 *
 * # Safety
 *
 * `c` precisa ser nulo ou vir de [`quall_canceller_new`] e não ter sido liberado.
 */
void quall_session_cancel(const struct QuallCanceller *c);

/**
 * Já pediram para cancelar? Nulo devolve `false`.
 *
 * # Safety
 *
 * `c` precisa ser nulo ou vir de [`quall_canceller_new`].
 */
bool quall_canceller_is_cancelled(const struct QuallCanceller *c);

/**
 * Libera o cancelador. Nulo é ignorado.
 *
 * Pode ser liberado a qualquer momento depois de a chamada bloqueante voltar: a chamada segura
 * a **própria** cópia da bandeira.
 *
 * # Safety
 *
 * `c` precisa vir de [`quall_canceller_new`] e não pode ter sido liberado antes.
 */
void quall_canceller_free(struct QuallCanceller *c);

/**
 * Sobe uma sessão como **emissor**: abre a sinalização, espera um receptor, pareia e oferece.
 *
 * **Bloqueia** até a sessão subir ou o prazo estourar. Chame de uma thread de trabalho.
 *
 * Anunciar por mDNS é separado, em [`quall_advertiser_start`], para que a casca possa oferecer
 * só o caminho por IP em rede que bloqueia multicast.
 *
 * # Safety
 *
 * `opcoes` precisa apontar para um [`QuallSessionOptions`] válido, com as strings vivas durante
 * a chamada.
 */
struct QuallSession *quall_host(const struct QuallSessionOptions *opcoes);

/**
 * Igual a [`quall_host`], com um cancelador. Ver [`QuallCanceller`] e a dívida 10.
 *
 * `cancelador` nulo é aceito e reproduz [`quall_host`] exatamente — assim a casca migra quando
 * quiser, sem mudar nada do que já funciona.
 *
 * # Safety
 *
 * `opcoes` precisa apontar para um [`QuallSessionOptions`] válido; `cancelador` precisa ser nulo
 * ou vir de [`quall_canceller_new`] e continuar vivo durante a chamada.
 */
struct QuallSession *quall_host_cancelable(const struct QuallSessionOptions *opcoes,
                                           const struct QuallCanceller *cancelador);

/**
 * A porta em que a sinalização do emissor ficou. Útil quando `signaling_port` foi `0`.
 *
 * Devolve `0` se ainda não há servidor (sessão de receptor) ou se algo falhou.
 *
 * # Safety
 *
 * `s` precisa vir de [`quall_host`] ou [`quall_connect`].
 */
uint16_t quall_session_signaling_port(const struct QuallSession *s);

/**
 * Sobe uma sessão como **receptor**: conecta no endereço, pareia e responde.
 *
 * `endpoint` é `"192.168.1.131:7877"` ou só `"192.168.1.131"` (a porta padrão entra sozinha).
 * É a **mesma** função para o endereço que veio do mDNS e para o que o usuário digitou — de
 * propósito: o fallback de rede sem multicast não pode ser um caminho de código que ninguém
 * exercita.
 *
 * **Só endereço.** Um link `quall://<pin>@<host>:<porta>` é `QUALL_STATUS_INVALID`: leia-o com
 * [`quall_parse_endpoint_json`], ponha o PIN nas opções e conecte no endereço. A volta automática
 * depois de uma queda vai com o endereço e **sem** PIN (`docs/contrato-teleprompter.md` §11.1).
 * Porta 0 também é `INVALID`.
 *
 * **Bloqueia**. Chame de uma thread de trabalho.
 *
 * # Safety
 *
 * `endpoint` e `opcoes` precisam ser válidos durante a chamada.
 */
struct QuallSession *quall_connect(const char *endpoint,
                                   const struct QuallSessionOptions *opcoes);

/**
 * Igual a [`quall_connect`], com um cancelador. Ver [`QuallCanceller`] e a dívida 10.
 *
 * # Safety
 *
 * `endpoint` e `opcoes` precisam ser válidos durante a chamada; `cancelador` precisa ser nulo ou
 * vir de [`quall_canceller_new`].
 */
struct QuallSession *quall_connect_cancelable(const char *endpoint,
                                              const struct QuallSessionOptions *opcoes,
                                              const struct QuallCanceller *cancelador);

/**
 * Igual a [`quall_connect_cancelable`], dizendo ao emissor **a tela deste aparelho**, em pixels
 * do painel (sem orientação). O emissor que cria um monitor por receptor — a tela estendida do
 * Mac — usa isso para dar ao monitor o formato desta tela.
 *
 * `0` nos dois lados é "não digo" e se comporta exatamente como [`quall_connect_cancelable`].
 * Um lado fora de 1 a 16384 px é [`QuallStatus::Invalid`], e a sessão não sobe: um número desses é
 * defeito da casca, e mandá-lo adiante daria ao outro lado um monitor impossível.
 *
 * **Por chamada, e não um ajuste do processo** (revisão adversarial de 10/09/2026): o mesmo
 * processo pode hospedar e conectar — o app do Mac e o do Android fazem os dois papéis —, e um
 * valor global iria parar no `Welcome` de quem hospeda.
 *
 * # Safety
 *
 * As mesmas de [`quall_connect_cancelable`].
 */
struct QuallSession *quall_connect_with_screen(const char *endpoint,
                                               const struct QuallSessionOptions *opcoes,
                                               const struct QuallCanceller *cancelador,
                                               uint32_t width_px,
                                               uint32_t height_px);

/**
 * O aparelho do outro lado, como JSON (`device_id`, `display_name`, `protocol_version`,
 * `capabilities`, `identity_authenticated: true`). Padrão `(buf, cap)`.
 * ID e nome reais vêm do anúncio autenticado e cifrado; substituem a linha efêmera da descoberta.
 *
 * # Safety
 *
 * `s` precisa vir de [`quall_host`] ou [`quall_connect`].
 */
intptr_t quall_session_peer_json(const struct QuallSession *s,
                                 char *buf,
                                 uintptr_t cap);

/**
 * **Quantos candidatos caíram antes deste**, sem derrubar a espera. `0` no receptor, e `0` é o
 * caso normal também no emissor.
 *
 * Diferente de zero quer dizer que alguém conectou e sumiu no meio — o receptor que fecha o app,
 * o scanner de porta da LAN — e que a espera sobreviveu a isso. Até 01/09/2026 não sobrevivia:
 * um `WSAECONNRESET` no handshake fechava a porta da sinalização **em definitivo, com o processo
 * vivo**, e do outro lado só se via "depois que sai não conecta". Ver
 * `quall_core::session::hospedar`.
 *
 * Existe para que o conserto apareça no registro da casca em vez de sumir: descarte que ninguém
 * conta é como um defeito volta a viver escondido. Devolve `-1` em sessão nula.
 *
 * # Safety
 *
 * `s` precisa vir de [`quall_host`] ou [`quall_connect`].
 */
int64_t quall_session_descartados(const struct QuallSession *s);

/**
 * **Por onde a mídia está indo**, como JSON. Padrão `(buf, cap)`.
 *
 * Quatro campos, todos podendo ser `null`:
 *
 * ```json
 * {"local_candidate":"a=candidate:1 1 UDP 2122317823 192.168.1.131 62493 typ host",
 *  "remote_candidate":"a=candidate:1 1 UDP 2122317823 192.168.1.131 51698 typ host",
 *  "local_address":"192.168.1.131:62493",
 *  "remote_address":"192.168.1.131:51698"}
 * ```
 *
 * # `null` é "o ICE ainda não fechou", e não é erro
 *
 * Antes de o par de candidatos ser escolhido não há o que dizer, e os quatro campos vêm `null`.
 * A função continua devolvendo o tamanho do JSON, **não** `-1`: devolver negativo ali
 * confundiria "ainda não" com "falhou", e uma casca que tratasse os dois igual mostraria erro
 * numa sessão que está apenas subindo. Negativo aqui é só sessão nula ou falha de serialização.
 *
 * # Por que esta função existe
 *
 * Porque **uma corrida "pelo cabo" pode fechar pela Wi-Fi e parecer sucesso**. Medido nesta
 * bancada em 2026-09-01: duas pontas na mesma máquina, sinalização por `127.0.0.1`, e o par
 * escolhido foi `192.168.1.131 <-> 192.168.1.131` — a mídia saiu pelo rádio. O `quall-probe`
 * sempre soube disso porque lê o `Ready` do Rust; a casca não tinha nada equivalente, e foi
 * exatamente essa linha que explicou os 8,6 s de `docs/receptor-ios.md:243`.
 *
 * # O que ela não diz
 *
 * Não diz "cabo" nem "Wi-Fi": diz o endereço que o ICE escolheu. `169.254.x` e `192.168.42.x`
 * são **pistas** de cabo, não provas — um `169.254.x` também é o que sobra quando o DHCP falha.
 * Quem quiser o rótulo "pelo cabo" precisa de duas testemunhas que concordem: esta, e a escolha
 * que a própria casca fez ao pedir o enlace.
 *
 * # Safety
 *
 * `s` precisa vir de [`quall_host`] ou [`quall_connect`].
 */
intptr_t quall_session_path_json(const struct QuallSession *s,
                                 char *buf,
                                 uintptr_t cap);

/**
 * O pareamento foi novo (o usuário digitou PIN) ou retomado?
 *
 * # Safety
 *
 * `s` precisa vir de [`quall_host`] ou [`quall_connect`].
 */
bool quall_session_pairing_is_new(const struct QuallSession *s);

/**
 * O estado de pareamento atualizado, para a casca **persistir**. Padrão `(buf, cap)`.
 *
 * Sem gravar isto, o usuário digita o PIN de novo na próxima sessão. `known_json` é o que veio
 * em [`QuallSessionOptions::known_peers_json`]; passar nulo começa do zero e devolve só este
 * par.
 *
 * # Safety
 *
 * `s` precisa vir de [`quall_host`] ou [`quall_connect`]; `known_json` precisa ser nulo ou uma
 * string válida.
 */
intptr_t quall_session_known_peers_json(const struct QuallSession *s,
                                        const char *known_json,
                                        char *buf,
                                        uintptr_t cap);

/**
 * Quantas tracks de **saída** esta sessão abriu.
 *
 * # Safety
 *
 * `s` precisa vir de [`quall_host`] ou [`quall_connect`].
 */
uintptr_t quall_session_track_count(const struct QuallSession *s);

/**
 * Pega a track de saída de índice `idx`, na ordem de [`QuallSessionOptions::tracks`].
 *
 * O handle devolvido é do chamador: libere com [`quall_track_free`].
 *
 * # Safety
 *
 * `s` precisa vir de [`quall_host`] ou [`quall_connect`].
 */
struct QuallTrack *quall_session_track(const struct QuallSession *s, uintptr_t idx);

/**
 * Próxima track que chegou **do outro lado**, esperando até `timeout_ms`.
 *
 * Devolve nulo quando nada chegou a tempo — o que é estado normal, não erro. O receptor chama
 * num laço: uma sessão traz tela, câmera e microfone, e quem decide como compor é ele.
 *
 * O handle devolvido é do chamador: libere com [`quall_track_free`].
 *
 * # Safety
 *
 * `s` precisa vir de [`quall_host`] ou [`quall_connect`].
 */
struct QuallTrack *quall_session_next_track(struct QuallSession *s, uint32_t timeout_ms);

/**
 * **O detector de queda.** Olha se a sessão caiu, esperando até `timeout_ms`.
 *
 * # Por que a casca precisa disto
 *
 * Sem ele o único sinal de que a sessão morreu é [`quall_track_send_frame`] voltar a falhar — e
 * esse mesmo status é o estado **normal** enquanto o ICE ainda não fechou. Pior: o
 * `CONSENT_TIMEOUT` do libjuice é 30 000 ms, então durante meio minuto o envio continua
 * devolvendo `QUALL_STATUS_OK` com o receptor morto. São ~900 quadros capturados, encodados em
 * hardware e empacotados para o vazio, com a bateria de um celular pagando a conta.
 *
 * Este detector olha a **sinalização**, que sabe em milissegundos, e o transporte como rede de
 * segurança. Chame do mesmo laço da captura, com `timeout_ms` pequeno (0 a 10): a espera é
 * limitada e não entra no caminho do quadro.
 *
 * Uma vez `QUALL_SESSION_EVENT_DISCONNECTED`, sempre — a sessão não volta.
 *
 * # Uma thread só
 *
 * Esta função lê da sinalização e por isso pede `*mut`. Chame de **uma** thread, a mesma que
 * chama [`quall_session_next_track`]. Chamar das duas ao mesmo tempo é corrida de dados.
 *
 * # Safety
 *
 * `s` precisa vir de [`quall_host`] ou [`quall_connect`].
 */
enum QuallSessionEvent quall_session_next_event(struct QuallSession *s,
                                                uint32_t timeout_ms);

/**
 * **O caminho de volta do sinal.** O receptor conta ao emissor o que viu do enlace numa janela.
 *
 * Chame do **receptor**, da mesma thread que chama [`quall_session_next_event`] — a sinalização
 * não é acessada de duas threads, e a fronteira não põe cadeado no caminho quente para permitir
 * isso.
 *
 * Os cinco números são **deltas da janela**, nunca acumulados desde o começo da sessão. Ver
 * `quall_core::signaling::RelatoDoEnlace` para por que este caminho é a sinalização e não RTCP,
 * e o que essa escolha custa.
 *
 * `packets` é **o que o emissor mandou**: `packets_seen + packets_lost_for_real` da janela. O
 * denominador de uma taxa de perda é o que o emissor mandou; dividir pelo que chegou já inverteu
 * a conclusão de uma frente inteira desta bancada.
 *
 * Falhar aqui não é motivo para o receptor parar nada: um emissor de versão antiga ignora a
 * mensagem por conta própria, e um socket morto vai aparecer no detector de queda que já existe.
 *
 * # Safety
 *
 * `s` precisa vir de [`quall_connect`] e não pode ter sido liberado.
 */
enum QuallStatus quall_session_report_link(struct QuallSession *s,
                                           uint64_t ms,
                                           uint64_t packets,
                                           uint64_t lost,
                                           uint64_t suspect,
                                           uint64_t broken_idrs,
                                           uint64_t not_delivered);

/**
 * Consome o relato **mais recente** que o outro lado mandou. Chame do **emissor**.
 *
 * Escreve **seis** `uint64_t` em `out`, na ordem `{ms, packets, lost, suspect, broken_idrs,
 * not_delivered}`, e devolve `true`. Devolve `false` quando não há relato novo — que é o caso na maioria das
 * chamadas, e o caso **sempre** quando o outro lado é uma versão que não relata.
 *
 * Relato repetido não existe: uma leitura consome. E se dois tiverem chegado entre duas
 * chamadas, o mais velho é descartado — agir sobre uma janela de um segundo atrás depois de já
 * ter a de meio segundo é agir sobre o passado, e é assim que um laço fechado oscila.
 *
 * Esta função **também** enxerga um `Bye` ou um `Error` que chegue na mesma leitura e registra a
 * queda da sessão: uma casca que só chame esta função continua sabendo que a sessão caiu, por
 * [`quall_session_next_event`].
 *
 * # Safety
 *
 * `s` precisa vir de [`quall_host`]; `out` precisa apontar para **seis** `uint64_t` (o sexto,
 * `not_delivered`, entrou em 09/09/2026 — ver o comentário no corpo).
 */
bool quall_session_take_link_report(struct QuallSession *s,
                                    uint32_t timeout_ms,
                                    uint64_t *out);

/**
 * Cria um controlador de taxa com o teto dado, em bps.
 *
 * **`ceiling_bps` tem de ser o bitrate que o produto usaria sem controlador.** Ele nasce nesse
 * valor e nunca passa dele: o controlador só sabe tirar e devolver o que tirou. É isso, e não a
 * sintonia dos parâmetros, que garante que um enlace limpo não piora — em 5 GHz esta bancada
 * mediu 0 perda em 9 003 pacotes, e ali o controlador não tem o que fazer.
 *
 * Devolve nulo com `ceiling_bps` zero.
 */
struct QuallRate *quall_rate_new(uint32_t ceiling_bps);

/**
 * Alimenta uma janela e devolve o motivo. Escreve `out_bps` **só** com `Down` ou `Up`.
 *
 * `packets` é o que o emissor mandou na janela (vistos + perdidos), nunca o que chegou.
 *
 * # Safety
 *
 * `r` precisa vir de [`quall_rate_new`]; `out_bps` pode ser nulo.
 */
enum QuallRateReason quall_rate_sample(struct QuallRate *r,
                                       uint64_t ms,
                                       uint64_t packets,
                                       uint64_t lost,
                                       uint64_t suspect,
                                       uint64_t broken_idrs,
                                       uint64_t not_delivered,
                                       uint32_t *out_bps);

/**
 * Bitrate em vigor, em bps. Zero para controlador nulo.
 *
 * # Safety
 *
 * `r` precisa vir de [`quall_rate_new`].
 */
uint32_t quall_rate_current_bps(const struct QuallRate *r);

/**
 * O controlador chegou ao piso?
 *
 * É a **única** condição em que a resposta certa é para o usuário e não para o encoder: abaixo
 * do piso a imagem não vale a pena, e dizer isso é melhor que entregar lodo. A casca decide o
 * que mostrar.
 *
 * # Safety
 *
 * `r` precisa vir de [`quall_rate_new`].
 */
bool quall_rate_at_floor(const struct QuallRate *r);

/**
 * Janelas consideradas, descidas e subidas, para o relato. Escreve **três** `uint64_t`.
 *
 * # Safety
 *
 * `r` precisa vir de [`quall_rate_new`]; `out` precisa apontar para três `uint64_t`.
 */
void quall_rate_counters(const struct QuallRate *r, uint64_t *out);

/**
 * Libera o controlador. Nulo é ignorado.
 *
 * # Safety
 *
 * `r` precisa vir de [`quall_rate_new`] e não pode ter sido liberado antes.
 */
void quall_rate_free(struct QuallRate *r);

/**
 * Fecha a sessão, **espera os tratadores da casca saírem**, e libera. Nulo é ignorado.
 *
 * Libere as tracks **antes**: depois disto, um [`QuallTrack`] que a casca ainda segure passa a
 * devolver erro em vez de enviar. Não é falha de memória — o handle continua válido e os
 * contadores continuam legíveis —, mas a sessão acabou e nada mais atravessa.
 *
 * # Isto **é** uma barreira, e é o que autoriza liberar o `user_data`
 *
 * Com `QUALL_STATUS_OK`, esta função garante duas coisas ao voltar:
 *
 * 1. Nenhum tratador desta sessão — `quall_track_on_frame`, `quall_track_on_idr_request` —
 *    está rodando em thread nenhuma.
 * 2. Nenhum voltará a rodar.
 *
 * **A casca pode liberar o `user_data` na linha seguinte.** Era o contrato que faltava, e o
 * header anterior dizia o contrário com todas as letras: "mantenha o `user_data` vivo por conta
 * própria, ou proteja-o com um cadeado que o callback respeite".
 *
 * # Quando o status **não** é `QUALL_STATUS_OK`, não libere nada
 *
 * A sessão é fechada e liberada de qualquer forma; o que muda é só a promessa sobre os
 * tratadores. Dois casos:
 *
 * - `QUALL_STATUS_TIMEOUT`: passaram-se 2 segundos e um tratador da casca ainda não voltou.
 *   Isso é tratador que bloqueia, o que o contrato proíbe. O `user_data` precisa continuar
 *   vivo, e o defeito é do lado da casca.
 * - `QUALL_STATUS_INVALID`: esta chamada veio **de dentro de um tratador**. Esperar seria
 *   esperar por si mesma, e o processo penduraria; a fronteira recusa a espera em vez de
 *   travar. Feche a sessão de uma thread que não seja a do tratador.
 *
 * # Não feche a sessão de dentro de um tratador
 *
 * Além de não render barreira, destruir a conexão de dentro de uma thread da libdatachannel é
 * pedir para a própria biblioteca se enroscar. Levante uma bandeira e feche do laço da casca.
 *
 * # Safety
 *
 * `s` precisa vir de [`quall_host`] ou [`quall_connect`] e não pode ter sido liberado antes.
 */
enum QuallStatus quall_session_close(struct QuallSession *s);

/**
 * Libera os recursos globais da libdatachannel. Chame **uma vez**, na saída do processo.
 *
 * Num app comum dá para viver sem; num plugin de OBS que é descarregado e recarregado, não — a
 * biblioteca deixa threads vivas.
 *
 * # Feche todas as sessões antes
 *
 * A libdatachannel espera todos os objetos dela serem destruídos. Chamada com uma sessão viva,
 * `rtcCleanup()` apaga os mapas, espera 10 segundos, desiste e registra `Cleanup timeout` — ela
 * **volta**, mas deixa a thread de limpeza presa, e é essa thread que impede o processo de
 * morrer. (Texto corrigido em 2026-08-23: a versão anterior dizia "trava o processo, sem erro e
 * sem log", que era o sintoma na bancada e não o que `capi.cpp:1691` faz. Há log — procure por
 * `Cleanup timeout` antes de qualquer outra coisa.)
 *
 * Foi assim que a sonda ficou pendurada na bancada no M1, segurando a porta de sinalização e
 * fazendo a execução seguinte falhar com "Address already in use" — um sintoma três passos
 * distante da causa.
 *
 * **No Android, não chame.** Não existe "saída do processo": o Service para e o processo
 * continua, e `System.loadLibrary` não descarrega a `.so`. Não há hora segura, e não há o que
 * limpar.
 */
void quall_cleanup(void);

/**
 * O que esta track carrega.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_session_track`] ou [`quall_session_next_track`].
 */
enum QuallTrackKind quall_track_kind(const struct QuallTrack *t);

/**
 * Rótulo legível da track. Padrão `(buf, cap)`.
 *
 * # Safety
 *
 * `t` precisa ser válido; `buf` precisa ser nulo ou ter `cap` bytes.
 */
intptr_t quall_track_label(const struct QuallTrack *t, char *buf, uintptr_t cap);

/**
 * **`enviar_quadro` do contrato.** Empacota e solta um quadro.
 *
 * Volta quando os pacotes RTP já foram entregues ao transporte. Não há fila nossa no caminho e
 * nenhuma cópia é guardada: é isso que mantém o núcleo dentro dos ~50 MB da Broadcast Upload
 * Extension do iOS.
 *
 * Erro aqui é a track ainda não estar aberta — estado normal enquanto o ICE não fechou — ou o
 * transporte ter caído. Nos dois casos a casca **descarta o quadro e segue**; enfileirar para
 * tentar de novo é exatamente o que não se deve fazer com vídeo ao vivo.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_session_track`]; `frame` precisa apontar para `len` bytes válidos
 * durante a chamada.
 */
enum QuallStatus quall_track_send_frame(const struct QuallTrack *t,
                                        const struct QuallFrame *frame);

/**
 * **`enviar_audio` do contrato.** Empacota e solta **um** quadro de áudio.
 *
 * # A porta que faltava
 *
 * `QUALL_TRACK_KIND_MICROPHONE` e `QUALL_TRACK_KIND_SYSTEM_AUDIO` existiam nesta fronteira e
 * **não tinham como ser usados**: a única porta de envio era [`quall_track_send_frame`], que
 * leva `QuallFrame` e devolve `QUALL_STATUS_INVALID` em track de áudio. `enviar_audio` parava
 * em `quall-core`; a sonda fala Rust direto, e por isso ninguém tropeçou. Uma casca C, Swift ou
 * Kotlin declarava a track e ficava sem o que fazer com ela.
 *
 * # Um quadro por chamada, e o erro não denuncia
 *
 * O pacotizador de áudio da libdatachannel **não fragmenta**: uma mensagem entra, um pacote RTP
 * sai. Dois quadros de Opus concatenados numa chamada viram um pacote que o outro lado
 * decodifica errado **sem erro nenhum no caminho**, porque para o RTP é só um payload maior.
 * [`MAX_AMOSTRA`] pega o caso grosseiro; o caso de dois quadros de 80 bytes ele não pega, e
 * nada pega — a regra é do chamador.
 *
 * Como no vídeo: não há fila nossa, nenhuma cópia é guardada, e erro aqui é a track ainda não
 * estar aberta ou o transporte ter caído. Nos dois casos a casca **descarta e segue**.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_session_track`]; `sample` precisa apontar para um
 * [`QuallAudioSample`] válido cujo `payload` tenha `len` bytes legíveis durante a chamada.
 */
enum QuallStatus quall_track_send_audio(const struct QuallTrack *t,
                                        const struct QuallAudioSample *sample);

/**
 * **O preset que o núcleo vai anunciar no SDP, para a casca ler antes de codificar.**
 *
 * # Por que isto existe, e por que é JSON
 *
 * A casca precisa saber taxa de amostragem, número de canais e amostras por quadro para
 * capturar e codificar. Se ela **fixar** esses números no código dela, o preset do fio e o
 * preset da captura viram duas fontes de verdade que divergem em silêncio — que é exatamente a
 * classe de defeito que o `useinbandfec=1` sem LBRR e o SPS sem `bitstream_restriction`
 * custaram a este projeto. Aqui a casca **pergunta**, e a resposta sai da mesma
 * `TrackKind::preset_de_audio` que gera o `a=fmtp`.
 *
 * JSON pelo precedente de [`quall_browser_devices_json`] e [`quall_track_stats_json`]: a
 * alternativa seria meia dúzia de funções com um campo cada, e cada campo novo do preset
 * obrigaria a uma função nova na fronteira.
 *
 * Serve **antes** de haver sessão, de propósito: quem vai mandar G.711 em Swift puro — o uso
 * que `docs/audio.md` §2 reservou ao PCMU — precisa dos números na hora de abrir a captura, e
 * não precisa de encoder nenhum.
 *
 * As chaves: `codec`, `sample_rate_hz`, `channels`, `frame_ms`, `frame_samples` (**por
 * canal**), `bitrate_bps`, `fec`, `expected_loss_pct`, `is_speech`, `payload_type`, `fmtp`,
 * `content_delay_us`: quanto o conteúdo decodificado sai atrás do carimbo (6 500 no Opus, o
 * lookahead do codificador do emissor; 0 no PCMU). O receptor o soma ao atraso interno dele,
 * como soma o do filtro do PCMU.
 *
 * Devolve `-1` e um erro em `quall_last_error` se a espécie não for de áudio. Padrão
 * `(buf, cap)`: com `buf` nulo devolve o tamanho necessário, incluindo o NUL.
 *
 * # Safety
 *
 * `buf` precisa ser nulo ou apontar para `cap` bytes graváveis.
 */
intptr_t quall_audio_preset_json(enum QuallTrackKind kind,
                                 enum QuallAudioCodec codec,
                                 char *buf,
                                 uintptr_t cap);

/**
 * **Cria um encoder de Opus já configurado pelo preset da espécie.**
 *
 * # Por que o encoder atravessa a fronteira
 *
 * Hoje só `quall-probe` depende de `quall-opus`, então o `libquall.a` que as cascas linkam **não
 * carrega símbolo nenhum da libopus**. E o macOS não tem encoder de Opus no sistema — o
 * AudioToolbox tem AAC, não Opus. Sem esta porta, nenhuma casca Apple codifica Opus, nunca.
 *
 * # A regra do desenho: uma fonte de verdade
 *
 * O encoder é configurado pela **mesma** `TrackKind::preset_de_audio` que gera o `a=fmtp` — taxa
 * de bits, canais, FEC, perda esperada e sinal saem todos de lá. Se cada casca configurasse o
 * seu, o preset do fio e o preset do encoder seriam duas fontes de verdade divergindo em
 * silêncio, e a classe de defeito que custou a §11 do `docs/audio.md` voltaria em quatro
 * lugares em vez de um.
 *
 * Em particular, `OPUS_SET_PACKET_LOSS_PERC` é setado aqui a partir de
 * `PresetDeAudio::perda_esperada_pct`. **Sem ele, `useinbandfec=1` produz um fluxo sem LBRR
 * nenhum** — medido, 0 de 300 pacotes — e o receptor do outro lado dimensionaria o jitter
 * buffer contando com uma recuperação que nunca viria.
 *
 * # Quando devolve nulo
 *
 * - a espécie não é de áudio;
 * - o preset resolvido pede [`QuallAudioCodec::Pcmu`]. **Isso é deliberado**: G.711 µ-law é uma
 *   tabela de consulta de 8 bits que a casca escreve em vinte linhas, e trazê-la para cá seria
 *   uma porta a mais na fronteira para um problema que não é dela. O erro diz isso;
 * - a biblioteca foi construída sem a feature `opus`.
 *
 * Sempre há um motivo legível em `quall_last_error`.
 *
 * # Safety
 *
 * A função em si não desreferencia nada. O ponteiro devolvido é do chamador e precisa ir para
 * [`quall_audio_encoder_free`].
 */
struct QuallAudioEncoder *quall_audio_encoder_new(enum QuallTrackKind kind,
                                                  enum QuallAudioCodec codec);

/**
 * **Muda a complexidade do encoder** (0–10, `OPUS_SET_COMPLEXITY`; acima de 10 vale 10) com ele em uso — a casca baixa no
 * calor e volta ao padrão da espécie depois (`docs/teleprompter-com-camera.md` §8.12.17). Não mexe
 * no fio. `0` = OK; `-1` com o motivo em `quall_last_error` (encoder nulo, ou a libopus recusou).
 *
 * # Safety
 *
 * `e` precisa vir de [`quall_audio_encoder_new`] e não ter sido liberado, e **não pode estar sendo
 * usado por outra thread ao mesmo tempo** (a mesma regra de [`quall_audio_encoder_encode`]).
 */
int32_t quall_audio_encoder_set_complexity(struct QuallAudioEncoder *e,
                                           uint8_t complexity);

/**
 * A complexidade padrão do encoder desta espécie de track (`PresetDeAudio::complexidade_do_encoder`),
 * para a casca voltar a ela; `-1` se a espécie não é de áudio.
 */
int32_t quall_audio_default_complexity(enum QuallTrackKind kind);

/**
 * Codifica **um** quadro de PCM intercalado de 16 bits.
 *
 * `pcm_len` é o número total de amostras — `frame_samples × channels`, os dois vindos de
 * [`quall_audio_preset_json`]. Devolve quantos bytes foram escritos em `out`, ou `-1` com o
 * motivo em `quall_last_error`.
 *
 * O que sai é exatamente o que vai em [`QuallAudioSample::payload`]: um quadro, um pacote.
 *
 * # Safety
 *
 * `e` precisa vir de [`quall_audio_encoder_new`] e não ter sido liberado. `pcm` precisa ter
 * `pcm_len` amostras legíveis; `out` precisa ter `out_cap` bytes graváveis. **Não é seguro
 * chamar de duas threads sobre o mesmo encoder** — o estado do Opus é preditivo.
 */
intptr_t quall_audio_encoder_encode(struct QuallAudioEncoder *e,
                                    const int16_t *pcm,
                                    uintptr_t pcm_len,
                                    uint8_t *out,
                                    uintptr_t out_cap);

/**
 * Libera o encoder. Nulo é no-op.
 *
 * # Safety
 *
 * `e` precisa vir de [`quall_audio_encoder_new`] e não pode ser liberado duas vezes.
 */
void quall_audio_encoder_free(struct QuallAudioEncoder *e);

/**
 * **Cria um decodificador de Opus já configurado pelo preset da espécie.**
 *
 * # Por que ele entrou agora, e não na rodada que trouxe o encoder
 *
 * `docs/audio.md` §13 recusou esta porta com três argumentos, e escreveu o que a faria entrar:
 * *"Prefiro entregá-la na rodada em que houver uma casca reproduzindo som."* É esta rodada — o
 * receptor Android toca o slot num `AudioTrack`, e sem esta porta ele não teria como.
 *
 * O argumento que continua **não** valendo é o que subiu o encoder: `opus_decode` não é
 * configurado por preset nenhum, então não há segunda fonte de verdade para divergir. O que
 * vale é o resto do parágrafo daquela seção — e a alternativa, no Android, seria o `MediaCodec`,
 * que **não tem** `decode_fec` nem ocultação de perda explícita. Com ele, a ordem
 * [`QuallAudioOrder::Fec`] que o jitter buffer entrega não teria consumidor: o socorro
 * atravessaria a rede e a casca o jogaria fora. A porta existe para que as três ordens do slot
 * tenham as três respostas.
 *
 * Ela **não** faz o núcleo tocar em PCM: quem escreve o `i16` é o chamador, no buffer dele. O
 * que atravessa continua sendo bytes de um lado e amostras do outro, e nenhuma decisão de
 * apresentação — taxa do dispositivo, mistura, WSOLA — mora aqui.
 *
 * # Quando devolve nulo
 *
 * Os mesmos três casos de [`quall_audio_encoder_new`], **e pelos mesmos motivos**:
 *
 * - a espécie não é de áudio;
 * - o preset resolvido pede [`QuallAudioCodec::Pcmu`]. G.711 µ-law é a mesma tabela de consulta
 *   de 8 bits na volta, e trazê-la para cá seria uma porta a mais para um problema que não é
 *   daqui. Numa track de PCMU a casca decodifica em vinte linhas e, na ordem `SILENCE`, escreve
 *   silêncio;
 * - a biblioteca foi construída sem a feature `opus`.
 *
 * Sempre há um motivo legível em `quall_last_error`.
 *
 * # Safety
 *
 * A função em si não desreferencia nada. O ponteiro devolvido é do chamador e precisa ir para
 * [`quall_audio_decoder_free`].
 */
struct QuallAudioDecoder *quall_audio_decoder_new(enum QuallTrackKind kind,
                                                  enum QuallAudioCodec codec);

/**
 * Decodifica **um** slot de 20 ms em PCM intercalado de 16 bits.
 *
 * As três ordens de [`QuallAudioOrder`] têm as três chamadas, e é isto que elas viram:
 *
 * | ordem do slot | `packet` | `decode_fec` | o que acontece |
 * |---|---|---|---|
 * | `FRAME` | o quadro | `false` | `opus_decode` normal |
 * | `FEC` | o pacote *N+1* | `true` | o LBRR reconstrói o slot que faltou |
 * | `SILENCE` | **nulo**, `len` 0 | `false` | ocultação de perda (PLC) do próprio decoder |
 *
 * **`decode_fec = true` só depois de conferir `fec_has_lbrr == 1` no slot.** Sem LBRR o
 * `opus_decode` cai na ocultação de perda em silêncio e devolve sucesso — esta porta não tem
 * como distinguir isso e não finge que tem. É a armadilha que `QuallAudioSlot::fec_has_lbrr`
 * existe para fechar.
 *
 * `out_cap` é o número de **amostras** (`i16`) graváveis, não bytes: para o preset de sistema
 * (estéreo, 20 ms a 48 kHz) são 1920. Devolve quantas amostras **por canal** foram escritas —
 * multiplique por `channels` do preset para saber quantos `i16` valem —, ou `-1` com o motivo em
 * `quall_last_error`.
 *
 * # Safety
 *
 * `d` precisa vir de [`quall_audio_decoder_new`] e não ter sido liberado. `packet` precisa ser
 * nulo ou ter `len` bytes legíveis; `out` precisa ter `out_cap` amostras graváveis. **Não é
 * seguro chamar de duas threads sobre o mesmo decodificador** — o estado do Opus é preditivo,
 * na volta como na ida.
 */
intptr_t quall_audio_decoder_decode(struct QuallAudioDecoder *d,
                                    const uint8_t *packet,
                                    uintptr_t len,
                                    bool decode_fec,
                                    int16_t *out,
                                    uintptr_t out_cap);

/**
 * Libera o decodificador. Nulo é no-op.
 *
 * # Safety
 *
 * `d` precisa vir de [`quall_audio_decoder_new`] e não pode ser liberado duas vezes.
 */
void quall_audio_decoder_free(struct QuallAudioDecoder *d);

/**
 * **O codec que esta track de áudio negociou**, lido do `a=rtpmap` da descrição dela.
 *
 * # Por que a casca precisa disto, e por que o preset não basta
 *
 * [`quall_audio_preset_json`] responde *"o que a espécie X com o codec Y pede"*, e serve ao
 * emissor, que **escolhe** o codec. O receptor não escolhe: ele recebe o que o outro lado
 * ofereceu, e `docs/audio.md` §6 diz que `TrackReceptor::adotar` lê o codec do `a=rtpmap` e
 * **recusa** a track quando não há um reconhecível. Sem esta função a casca receptora teria de
 * adivinhar entre Opus e G.711 — e adivinhar errado não dá erro em lugar nenhum: sai som, com o
 * relógio numa escala 6× errada. É a mesma classe de defeito que o campo de codec do
 * [`QuallTrackDesc`] existe para fechar, do lado de cá.
 *
 * Devolve [`QuallAudioCodec::Default`] (`0`) quando **não há resposta**: track nula, track de
 * emissão, track de vídeo, ou track de áudio que ainda não adotou codec nenhum. O `0` aqui é
 * *"não sei"*, e não *"o padrão"* — o motivo sai em `quall_last_error`, na mesma thread.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_session_track`] ou [`quall_session_next_track`].
 */
enum QuallAudioCodec quall_track_audio_codec(const struct QuallTrack *t);

/**
 * **A porta de recepção de áudio.** Registra o tratador que recebe os slots já ordenados pelo
 * jitter buffer do núcleo.
 *
 * # Por que ela existe, e por que ela não entrega o pacote cru
 *
 * Antes desta rodada a fronteira C tinha o pacote de **envio** de áudio e nenhuma recepção:
 * [`quall_track_on_frame`] é só de vídeo, então **uma casca C conseguia mandar áudio e não
 * conseguia receber**. Metade de um par.
 *
 * A porta óbvia — entregar o pacote RTP cru, espelhando `quall_track_on_frame` — seria a
 * errada, e o documento de áudio já tinha pago para descobrir isso. O jitter buffer **mudou de
 * lado** em 2026-08-27: morava na casca, e o argumento que o trouxe para o núcleo é o mesmo que
 * decide aqui — *"sem esse campo, quatro cascas reimplementariam a conta de sequência sobre os
 * carimbos, de quatro jeitos"*. Entregar o pacote cru em C convidaria as quatro a fazer
 * exatamente isso, e desfaria a decisão pela porta dos fundos.
 *
 * Áudio também não tem a saída que o vídeo tem: **não existe quadro-chave de áudio** e o
 * sumidouro é um DAC, que consome 48 000 amostras por segundo para sempre. O que sai daqui é
 * uma ordem por slot de 20 ms, **sempre, sem buraco**.
 *
 * # A política não é parâmetro, e isso é a mesma decisão do encoder
 *
 * A profundidade e o `fec_disponivel` saem do **preset da track** e do codec lido no
 * `a=rtpmap` — as mesmas fontes que geram o `fmtp` do SDP e configuram
 * [`quall_audio_encoder_new`]. Deixar a casca escolher reintroduziria em quatro lugares a
 * classe de defeito da §11 de `docs/audio.md`: o preset do fio e o do buffer divergindo em
 * silêncio. **Uma fonte de verdade, no núcleo.**
 *
 * Em particular, `fec_disponivel` **não** é o `fec` cru do preset: numa track de microfone
 * negociada em PCMU o preset ainda diz `fec: true`, e oferecer socorro num fluxo de G.711 faria
 * a casca chamar `decode_fec` num payload sem LBRR nenhum.
 *
 * # Com a reprodução puxada aberta, devolve `INVALID`
 *
 * As duas portas são exclusivas por track (`docs/contrato-som-puxado.md` §2). Com
 * [`quall_audio_playout_new`] aberto, registrar **e** desregistrar aqui devolvem
 * `QUALL_STATUS_INVALID` sem mexer em nada.
 *
 * # Desregistrar **escoa o buffer**, e é por isso que a ordem importa
 *
 * `cb` nulo desliga o tratador. Antes de voltar, esta função entrega os slots que ainda estavam
 * retidos — chamando o tratador **antigo**, **desta thread**. Sem isso os últimos
 * `profundidade` slots de toda sessão sumiriam: 40 ms que atravessaram a rede e nunca tocaram,
 * que apareceriam numa tabela como dois quadros a menos sem ninguém saber de onde vieram.
 *
 * **Consequência para quem chama: não libere o `user_data` antes desta chamada voltar.** Ao
 * voltar com `QUALL_STATUS_OK` valem as duas garantias de sempre — o tratador não está rodando
 * em thread nenhuma e não voltará a rodar —, e aí o `user_data` pode ir embora.
 *
 * O escoamento chama o tratador **sem** nenhum cadeado desta track na mão: de dentro dele a
 * casca pode chamar qualquer função da track. **Tudo o que ele usa tem de estar vivo até esta
 * chamada voltar**: o `user_data`, e todo handle que ele consulte, inclusive o de outra track
 * (ver a ordem de fechar em [`quall_track_free`]).
 *
 * # De dentro de um tratador
 *
 * Chamada de dentro de um tratador da sessão, devolve `QUALL_STATUS_INVALID` (a barreira não
 * vale: não libere o `user_data`), e o tratador fica desligado.
 * - De dentro do tratador de áudio **desta** track, os slots retidos **se perdem**: escoá-los
 *   pediria o cadeado que o próprio tratador segura.
 * - De dentro do tratador de **outra** track, eles são escoados normalmente.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_session_next_track`]; `cb` precisa ser válido ou nulo.
 */
enum QuallStatus quall_track_on_audio(const struct QuallTrack *t,
                                      QuallAudioSlotCallback cb,
                                      void *user_data);

/**
 * **Abre a reprodução puxada** de uma track receptora de áudio.
 *
 * `shell_resamples = true` diz que a casca aplica a razão sugerida
 * ([`quall_audio_playout_rate`]): Varispeed, `RATEADJUST`, o período da thread do OBS. Nesse
 * modo o núcleo **nunca** descarta nem insere slot por deriva.
 *
 * Devolve nulo, com o motivo em `quall_last_error`, quando a track é nula, de emissão ou de
 * vídeo, quando ela já tem um tratador empurrado ([`quall_track_on_audio`]), ou quando já tem
 * uma reprodução puxada aberta. **As duas portas são exclusivas.**
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_session_next_track`] ou [`quall_session_track`] e continuar válido
 * durante a chamada. O ponteiro devolvido vai para [`quall_audio_playout_free`].
 */
struct QuallAudioPlayout *quall_audio_playout_new(const struct QuallTrack *t, bool shell_resamples);

/**
 * **Puxa o slot de 20 ms que sai agora.** Uma chamada por slot entregue ao dispositivo, na
 * cadência dele, **numa thread de cada vez**.
 *
 * - `delay_to_dac_us`: daqui a quanto a primeira amostra deste slot sai do DAC (a fila da casca,
 *   o buffer do dispositivo e a latência que ele declara).
 * - `applied_rate`: a razão de reamostragem que a casca **de fato** aplicou; `NAN` quando não
 *   sabe ou não reamostra. Sem ela a estimativa de deriva erraria exatamente pelo que a casca
 *   corrigiu.
 * - `out` recebe o slot. Com `QUALL_AUDIO_ORDER_IDLE`, escreva **zeros**. O `payload` vale **até
 *   a próxima chamada de `pull` ou de `free` sobre o mesmo `p`**.
 *
 * Devolve `QUALL_STATUS_OK`, ou `QUALL_STATUS_NULL_POINTER` com `p` ou `out` nulo.
 *
 * # Safety
 *
 * `p` precisa vir de [`quall_audio_playout_new`] e não ter sido liberado; `out` precisa apontar
 * para um `QuallAudioSlot` gravável.
 */
enum QuallStatus quall_audio_playout_pull(struct QuallAudioPlayout *p,
                                          uint32_t delay_to_dac_us,
                                          double applied_rate,
                                          struct QuallAudioSlot *out);

/**
 * A razão de reamostragem sugerida, perto de 1,0 e limitada a ±500 ppm. `NAN` quando **não
 * medida**: antes de 10 s de reprodução, e depois de toda reancoragem.
 *
 * **De qualquer thread**, concorrente com [`quall_audio_playout_pull`] — **mas nunca junto com
 * [`quall_audio_playout_free`]**: o `free` libera `p`, e ler depois é uso de memória liberada.
 *
 * # Safety
 *
 * `p` precisa vir de [`quall_audio_playout_new`] e não ter sido liberado.
 */
double quall_audio_playout_rate(const struct QuallAudioPlayout *p);

/**
 * Os contadores da reprodução puxada, como JSON. Padrão `(buf, cap)`. As chaves estão em
 * `docs/contrato-som-puxado.md` §4. **De qualquer thread**, mas **nunca junto com
 * [`quall_audio_playout_free`]**.
 *
 * # Safety
 *
 * `p` precisa vir de [`quall_audio_playout_new`] e não ter sido liberado; `buf` nulo ou com
 * `cap` bytes graváveis.
 */
intptr_t quall_audio_playout_stats_json(const struct QuallAudioPlayout *p,
                                        char *buf,
                                        uintptr_t cap);

/**
 * **Encerra a reprodução puxada.** `p` é liberado **sempre**, seja qual for o status.
 *
 * Tira o produtor do caminho do pacote, com barreira, e libera a track para outra porta.
 * `QUALL_STATUS_OK`: a barreira valeu. `QUALL_STATUS_TIMEOUT`: o prazo estourou.
 * `QUALL_STATUS_INVALID`: chamada de dentro de um tratador da sessão. Esta porta não tem
 * `user_data`, então o status só informa.
 *
 * **Nunca concorrente com nenhuma chamada sobre o mesmo `p`**: nem `pull`, nem `rate`, nem
 * `stats_json`. A casca que lê estatísticas num temporizador para o temporizador antes de
 * liberar (revisão do código da S1, A5).
 *
 * # Safety
 *
 * `p` precisa vir de [`quall_audio_playout_new`] e não ter sido liberado antes. Nulo é no-op.
 */
enum QuallStatus quall_audio_playout_free(struct QuallAudioPlayout *p);

/**
 * **O deslocamento de captura** desta track no relógio comum da sessão.
 *
 * `timestamp_us` de um quadro ou slot desta track, mais `*out_us`, é o instante de captura em
 * µs desde a época da sessão — comum a todas as tracks receptoras dela. Ver
 * `docs/som-no-receptor.md` §5.
 *
 * Devolve `1` (válido, `*out_us` escrito), `0` (ainda não medido) ou `-1` (recusado pela guarda
 * do relógio, taxa não suportada ou erro; o motivo em `quall_last_error`). **Pode passar de `1`
 * para `-1` no meio da sessão**: consulte de novo.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_session_next_track`] ou [`quall_session_track`]; `out_us` precisa
 * apontar para um `int64_t` gravável.
 */
int32_t quall_track_capture_offset_us(const struct QuallTrack *t, int64_t *out_us);

/**
 * Deslocamento de captura para gravação. A guarda da reprodução não suprime este valor.
 * Retorna 1 com saídas escritas, 0 enquanto a medida não existe, -1 para erro. `out_guard`
 * recebe 1 (válida), 0 (pendente) ou -1 (recusada). As saídas não mudam com retorno 0/-1.
 * Não usar no render de áudio: esta consulta toma o cadeado do relógio da sessão.
 *
 * # Safety
 * Handle receptor válido e duas saídas graváveis, distintas.
 */
int32_t quall_track_capture_offset_raw_us(const struct QuallTrack *t,
                                          int64_t *out_us,
                                          int32_t *out_guard);

/**
 * **`ao_pedir_idr` do contrato.** Registra o tratador do pedido de IDR do receptor.
 *
 * Disparado quando chega **PLI ou FIR**. A casca responde forçando um IDR pelo meio que a
 * plataforma dela permitir — e no Windows, hoje, isso significa recriar o MFT, com o custo
 * medido de ~150 ms. **Ignorar o pedido é deixar o receptor sem imagem**: é falha de produto,
 * não detalhe.
 *
 * O tratador roda numa **thread da libdatachannel**, não na sua. Bloquear nele segura a
 * recepção de RTCP da sessão inteira; o certo é levantar uma bandeira que o laço de captura
 * leia.
 *
 * # Tempo de vida do `user_data`, e como desligar
 *
 * `user_data` é repassado como veio. Ele precisa continuar válido até uma destas duas coisas
 * acontecer, o que vier primeiro:
 *
 * - **`cb` nulo desregistra**: chame esta mesma função com `cb = NULL` e, com
 *   `QUALL_STATUS_OK`, o tratador antigo não está rodando em thread nenhuma e não voltará a
 *   rodar. É a saída para quem fecha uma fonte sem derrubar a sessão.
 * - **[`quall_session_close`] com `QUALL_STATUS_OK`**, que é barreira para a sessão inteira.
 *
 * Note que **não é** o tempo de vida do handle: [`quall_track_free`] solta só a referência da
 * casca e deixa o tratador armado, de propósito, porque dois handles podem apontar para a mesma
 * track.
 *
 * Status diferente de `QUALL_STATUS_OK` no desregistro significa **não libere o `user_data`** —
 * ver [`quall_session_close`] para os dois casos.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_session_track`]; `cb` precisa ser uma função válida ou nulo.
 */
enum QuallStatus quall_track_on_idr_request(const struct QuallTrack *t,
                                            QuallIdrRequestCallback cb,
                                            void *user_data);

/**
 * Alternativa a [`quall_track_on_idr_request`]: **consome** um pedido pendente.
 *
 * Devolve `true` no máximo uma vez por rajada de PLI/FIR, e baixa a bandeira. O laço de captura
 * chama uma vez por quadro e, quando vier `true`, força um IDR.
 *
 * # Em Android, prefira esta
 *
 * O tratador de [`quall_track_on_idr_request`] roda numa thread da libdatachannel, que **não
 * está anexada à JVM**. Chamar de volta para o Kotlin de lá exige `AttachCurrentThread`,
 * referência global e desanexar na saída — três chances de derrubar o app, num aparelho de
 * 1,79 GB, por causa de um pedido de quadro-chave. Com esta função a casca Android não precisa
 * de callback nenhum: ela já tem um laço por quadro, o do MediaCodec, e uma leitura atômica
 * por quadro não custa nada.
 *
 * As duas formas convivem: registrar o tratador não desliga a bandeira.
 *
 * Devolve `false` para track nula ou de recepção.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_session_track`].
 */
bool quall_track_take_idr_request(const struct QuallTrack *t);

/**
 * **`ao_receber_quadro` do contrato.** Registra o tratador de quadro remontado.
 *
 * O tratador roda numa **thread da libdatachannel** e recebe um `QuallFrame` cujo `annexb`
 * aponta para o buffer interno do núcleo, válido **só durante a chamada**. No caminho normal a
 * casca entrega direto ao decoder, sem copiar; se precisar guardar, copie ali.
 *
 * Vale aqui a mesma regra de tempo de vida de [`quall_track_on_idr_request`], **inclusive o
 * desregistro**: `cb` nulo desliga o tratador e, com `QUALL_STATUS_OK`, garante que o antigo
 * não está rodando em thread nenhuma. Depois disso o `user_data` pode ser liberado, mesmo com a
 * sessão de pé.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_session_next_track`]; `cb` precisa ser válido ou nulo.
 */
enum QuallStatus quall_track_on_frame(const struct QuallTrack *t,
                                      QuallFrameCallback cb,
                                      void *user_data);

/**
 * **`pedir_idr` do contrato.** Pede um IDR ao emissor, emitindo PLI.
 *
 * A casca receptora chama ao entrar na sessão sem ter visto IDR, ou quando o decoder perde
 * sincronia. Devolve erro enquanto a track não abriu — e engolir isso em silêncio seria
 * reproduzir, do lado do receptor, o defeito que o contrato existe para resolver. Tentar de
 * novo por alguns milissegundos é o comportamento certo.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_session_next_track`].
 */
enum QuallStatus quall_track_request_idr(const struct QuallTrack *t);

/**
 * **Quantos quadros o depacotizador jogou fora por estarem incompletos.**
 *
 * Existe separado de `quall_track_stats_json` porque é lido **no laço de recepção**, e não no
 * relato: é o gatilho que faz o receptor pedir um IDR quando a cadeia de referência se rompe.
 * Montar e desserializar um JSON a cada volta para ler um `u64` seria pagar um alocador por
 * quadro — e uma casca que ache isso caro vai acabar não perguntando, que é como o receptor iOS
 * ficou pedindo IDR **uma única vez por sessão** enquanto a imagem se desfazia.
 *
 * Cada unidade aqui é uma ruptura da cadeia: um quadro P decodificado contra uma referência que
 * nunca chegou deixa rastro do que se move e suja macroblocos, até o IDR seguinte. Quem lê isto
 * e não pede reparo está escolhendo o rastro.
 *
 * Devolve 0 para track nula ou de emissão — o emissor não remonta nada, então não tem o que
 * descartar. **0 não é erro**, e por isso esta função não escreve em `quall_last_error`.
 *
 * # Safety
 *
 * `t` precisa ser uma track viva desta fronteira, ou nulo.
 */
uint64_t quall_track_frames_dropped(const struct QuallTrack *t);

/**
 * Contadores da track, como JSON. Padrão `(buf, cap)`.
 *
 * **Desde 18/09/2026**, numa track receptora, também `clock` (o relógio comum da sessão) e
 * `jitter_buffer` (os contadores da porta empurrada), com as chaves de
 * `docs/contrato-som-puxado.md` §4.
 *
 * No emissor: `frames_sent`, `idrs_sent`, `idrs_without_parameters`, `idr_requests`,
 * `buffered_bytes`.
 * No receptor: `frames_ready`, `frames_dropped`, `idrs_ready`, `idrs_broken`,
 * `largest_frame_ready_packets`, `largest_broken_frame_packets_received`, `sequence_anomalies`,
 * `packets_missing_upper_bound`, `reorder_events`, `packets_seen`, `packets_lost_for_real`,
 * `packets_too_late`, `rtcp_ignored`, `idr_requests`, `jitter_us`.
 *
 * # MUDANÇA DE CONTRATO EM 2026-08-29: `packets_missing` virou `packets_missing_upper_bound`
 *
 * A chave `packets_missing` **não existe mais**, e não há alias. Quem a lia lê agora
 * `packets_missing_upper_bound`, com exatamente o mesmo valor e o mesmo significado — o que
 * mudou é só o nome dizer o que o número sempre foi.
 *
 * O motivo é medido, não estético. Aquele número nunca foi perda: numa corrida com 486 nele, o
 * emissor tinha entregado 27.779 pacotes e o receptor visto 27.729 — sumiram **cinquenta**. O
 * resto era reordenação, cobrada como perda. Isso estava escrito nesta docstring desde a dívida
 * 26 e mesmo assim foi lido como perda em **todas** as medições desta bancada, porque quem lê um
 * contador lê o nome dele, não a documentação dele.
 *
 * **Não foi mantido alias de propósito.** Um `packets_missing` sobrevivente ao lado do nome novo
 * seria exatamente a armadilha que a renomeação existe para tirar. O preço é que um leitor não
 * migrado passa a ler a chave como ausente; todos os leitores desta árvore foram migrados no
 * mesmo commit (`quall-probe` e o app Windows leem o campo Rust, não o JSON, e não mudaram).
 *
 * **`jitter_us` é `null` quando não foi medido**, e não zero: em track de vídeo ele nunca é
 * calculado, e em track de áudio só existe a partir do segundo pacote.
 *
 * **`idrs_without_parameters` diferente de zero é defeito da casca emissora**: o contrato manda
 * todo IDR levar SPS e PPS, e sem isso quem entra na sessão depois fica sem imagem — que é
 * exatamente o defeito medido no Windows no M1.
 *
 * # Perdeu ou reordenou?
 *
 * `sequence_anomalies` sempre somou as duas coisas, e continua somando —
 * `sequence_anomalies == packets_missing_upper_bound + reorder_events`, sempre. Quem precisa da
 * resposta lê os dois separados:
 *
 * - **`packets_missing_upper_bound`** é a **cota superior** da perda: a soma dos saltos de
 *   sequência para a frente. Com `reorder_events == 0` ele é a perda **exata**; com ele
 *   diferente de zero é só um teto, porque cada pacote atrasado é contado no salto que passa por
 *   cima dele e de novo no salto de volta. **Não leia este número como perda** — o número da
 *   perda é o de baixo.
 * - **`reorder_events`** conta *eventos* — pacote repetido ou sequência andando para trás —, não
 *   pacotes.
 * - **`packets_lost_for_real`** é a perda **exata**, inclusive com reordenação: uma posição só
 *   entra aqui quando sai da janela de reordenação (128 posições) sem nunca ter chegado.
 *   `packets_too_late` diferente de zero denuncia que a janela foi curta e que este número está
 *   superestimado nesse tanto.
 *
 * **Quanto o teto infla, medido**: em 29/08, MacBook → A10s com origem sintética,
 * `packets_missing_upper_bound` = 486 (1,72 %) com 70 eventos fora de ordem, contra no máximo 50
 * pacotes que o emissor entregou e o receptor não viu. **Dez vezes.** Toda a matriz de perda
 * desta bancada, até essa data, leu o teto como se fosse a perda — ver
 * `docs/caminho-de-saida.md`.
 *
 * # `packets_seen`, e o que nenhum contador pode saber
 *
 * É a **janela observada**: quantos pacotes de mídia entraram na conta de sequência, incluindo
 * o primeiro. O primeiro pacote visto **fixa a linha de base**, e nada que tenha caído antes
 * dele pode ser contado — um número de sequência RTP não diz nada sobre o que veio antes do
 * primeiro que se viu. Por isso `sequence_anomalies == 0` nunca significou "nada se perdeu".
 *
 * Com ele a taxa vira conta local, e são **duas** taxas, não uma: a perda de verdade é
 * `packets_lost_for_real / (packets_lost_for_real + packets_seen)` e o teto é
 * `packets_missing_upper_bound / (packets_missing_upper_bound + packets_seen)`. Antes disso o
 * denominador precisava do contador de pacotes do sistema operacional da outra ponta. E
 * `packets_seen == 0` quer dizer que nenhum pacote chegou ainda: aí não se afirma nada.
 *
 * **Bancada: crava a profundidade do anel de reordenação desta track receptora**, em pacotes, e
 * desliga o ajuste automático. `0` desliga a fila inteira e reproduz o comportamento anterior a
 * 01/09/2026.
 *
 * # Por que a fronteira precisa disto
 *
 * O anel passou a se ajustar sozinho em 02/09/2026, e a primeira corrida de Wi-Fi levantou uma
 * dúvida contra o próprio ajuste: com a mesma perda (~3,8 %), o anel adaptativo terminou em 4 e
 * mediu mais que o dobro de `suspeitos` por mil quadros que a corrida do dia anterior com o anel
 * fixo em 16. Mas as duas corridas são de **dias diferentes**, e duas corridas de 2,4 GHz
 * separadas no tempo não se comparam. Sem braço de controle no mesmo enlace, a acusação é
 * anedota.
 *
 * Este é o braço de controle, e é **só** isso: `0` é o padrão de produto e mantém o ajuste
 * ligado. Nenhuma casca de produto chama esta função.
 *
 * Devolve `Invalid` numa track de emissor ou de áudio — nenhuma das duas tem anel.
 *
 * # Safety
 *
 * `t` precisa vir de `quall_session_next_track` e continuar válido.
 */
enum QuallStatus quall_track_set_reorder_depth(const struct QuallTrack *t,
                                               uint32_t pacotes);

/**
 * # Safety
 *
 * `t` precisa ser válido; `buf` precisa ser nulo ou ter `cap` bytes.
 */
intptr_t quall_track_stats_json(const struct QuallTrack *t, char *buf, uintptr_t cap);

/**
 * Libera o handle de track. Nulo é ignorado.
 *
 * Não fecha a track: ela vive com a sessão. Isto solta só a referência da casca.
 *
 * # E **não** desregistra o tratador
 *
 * É de propósito, e não descuido: [`quall_session_track`] pode ser chamado duas vezes e
 * devolver dois handles para a mesma track. Um deles sendo liberado não pode desligar o
 * tratador que o outro registrou.
 *
 * Para desligar o tratador antes de liberar o handle, chame [`quall_track_on_frame`] ou
 * [`quall_track_on_idr_request`] com `cb = NULL` e confira o status. É essa chamada, e não
 * esta, que autoriza liberar o `user_data`.
 *
 * # A ordem de fechar, quando um tratador consulta outra track
 *
 * A biblioteca não guarda o handle: depois de liberado, o que ela ainda chama são só os
 * tratadores registrados, com o `user_data` deles. **Mas um tratador da casca que consulta um
 * handle** — o de outra track, por exemplo, para o deslocamento de captura — o usa, e liberar
 * esse handle antes é uso de memória liberada **da casca**. Isso vale também para o escoamento
 * de [`quall_track_on_audio`] com `cb = NULL`, que chama o tratador de áudio desta thread.
 *
 * A ordem segura: desregistre **todos** os tratadores da sessão, confira os status, e só então
 * libere os handles. (Reconferência da S1: o SIGSEGV do revisor B era o tratador de áudio do
 * teste consultando a track de vídeo que o teste já tinha liberado.)
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_session_track`] ou [`quall_session_next_track`] e não pode ter
 * sido liberado antes.
 */
void quall_track_free(struct QuallTrack *t);

/**
 * Versão do protocolo de sinalização que este binário fala.
 */
uint16_t quall_protocol_version(void);

/**
 * Tipo de serviço mDNS anunciado na LAN, como C string estática.
 *
 * O ponteiro é válido pelo tempo de vida do processo e **não** deve ser liberado por quem chama.
 */
const char *quall_service_type(void);

/**
 * **Quanto deste quadro pode ir para a rede.** Pergunte antes de criar o encoder.
 *
 * Esta é a resposta única para as quatro cascas do projeto. Ela existe porque, até 2026-08-28,
 * cada casca decidia sozinha e três das quatro decidiam errado — medido, um emissor de cada vez:
 * Android mandava 720x1520 (nível 3.2), Windows 1920x1080 (4.0) e macOS 2560x1664@60 (5.2),
 * enquanto o SDP prometia 3.1 a todos eles. O iOS cabia, mas por código próprio, não por acordo.
 *
 * O teto sai do `profile-level-id` que o próprio núcleo anuncia, e a conta é a da norma — área
 * em macroblocos (`MaxFS`) e macroblocos por segundo (`MaxMBPS`) —, não uma caixa de 1280x720.
 * A diferença aparece em tela alongada: 720x1520 vira 652x1378 preservando a proporção, em vez
 * de ser espremido num retângulo 16:9 que ninguém pediu.
 *
 * Entrada zero não é erro: devolve o retângulo que satura o nível, que é a saída mais
 * conservadora possível. Quem chama está abrindo uma sessão e não tem o que fazer com uma
 * ausência.
 *
 * # Safety
 *
 * `saida` precisa ser um ponteiro gravável para um `QuallTeto`, ou nulo — nulo devolve
 * `NullPointer` sem escrever nada.
 */
enum QuallStatus quall_teto_ajustar(uint32_t largura,
                                    uint32_t altura,
                                    uint32_t fps,
                                    struct QuallTeto *saida);

/**
 * Como [`quall_teto_ajustar`], mas respeitando **a resolução que o usuário escolheu**.
 *
 * `alvo_max_fs` são os macroblocos por quadro que ele aceita emitir — 3600 para 720p, 8160 para
 * 1080p, 14400 para 2K, 32400 para 4K. `alvo_fps` é a taxa de quadros pedida.
 *
 * **`0` em qualquer um dos dois quer dizer "não escolheu"** e cai no padrão daquele eixo: 1080p e
 * 30 fps, que é o comportamento de antes do cardápio, byte a byte. Os dois zeros juntos são
 * exatamente [`quall_teto_ajustar`].
 *
 * Vale sempre **o menor** entre a escolha e o nível anunciado: pedir 4K num binário que anuncia
 * 4.0 devolve 1080p, e não um erro. O usuário pediu o máximo que o aparelho permitir, e é isso
 * que ele recebe — ver `quall_core::teto::Alvo`.
 *
 * # Safety
 *
 * Mesma regra de [`quall_teto_ajustar`]: `saida` precisa ser um ponteiro gravável para um
 * `QuallTeto`, ou nulo — nulo devolve `NullPointer` sem escrever nada.
 */
enum QuallStatus quall_teto_ajustar_para(uint32_t largura,
                                         uint32_t altura,
                                         uint32_t fps,
                                         uint32_t alvo_max_fs,
                                         uint32_t alvo_fps,
                                         struct QuallTeto *saida);

/**
 * O `level_idc` que o SDP deste binário anuncia — 31 para o nível 3.1.
 *
 * Serve para uma casca **conferir** o que o encoder dela produziu contra o que foi prometido,
 * que é a regra da casa: verificar no artefato, não no retorno da API. Ler o `level_idc` do SPS
 * que saiu e compará-lo com este número é uma linha de código e teria respondido a frente
 * inteira da tela preta no primeiro dia.
 */
uint8_t quall_teto_nivel_anunciado(void);

/**
 * Existe algum vínculo autenticado pela revisão segura v3? Registros legados não contam.
 *
 * Retorna `1`/`0`, ou `-1` se o JSON é inválido. Não identifica o aparelho remoto da descoberta.
 *
 * # Safety
 * `known_json` precisa ser nulo ou apontar para uma string UTF-8 terminada em zero.
 */
int32_t quall_known_peers_has_secure(const char *known_json);

/**
 * **Esquece um par.** Devolve o estado de pareamento sem ele, no padrão `(buf, cap)`.
 *
 * É o que a casca oferece como "parear de novo". Sem isto, um pareamento que dessincronizou não
 * tinha saída: o `Resume` morria em "não está pareado aqui", o produto não oferecia digitar o
 * PIN outra vez, e o usuário vivia o pior tipo de defeito — funcionou ontem, hoje não funciona.
 *
 * Par que não existe não é erro: devolve a tabela como estava.
 *
 * # Safety
 *
 * `known_json` e `device_id` precisam ser nulos ou strings válidas; `buf` precisa ser nulo ou
 * ter `cap` bytes.
 */
intptr_t quall_known_peers_forget(const char *known_json,
                                  const char *device_id,
                                  char *buf,
                                  uintptr_t cap);

/**
 * **Funde dois estados de pareamento**, no padrão `(buf, cap)`. União; na colisão vence o mais
 * recente.
 *
 * # Por que o núcleo oferece isto (dívida 23)
 *
 * O pareamento é chaveado só pelo `DeviceId` — o que é o comportamento **certo**, e é o que faz
 * "parear pela tela e depois usar a câmera" não pedir PIN de novo no iOS. O preço é que duas
 * origens do mesmo aparelho escrevem o mesmo arquivo.
 *
 * Enquanto o núcleo só entregava ler-modificar-escrever, uma atualização perdida bastava para
 * as duas origens ficarem com segredos diferentes sob a mesma chave, e a retomada seguinte
 * falhar duro. Com isto, a casca lê o disco, **funde** com o que tem na mão e grava — e uma
 * corrida perdida deixa de apagar uma entrada e passa a convergir para a mais recente, que é
 * justamente a que o outro lado guardou.
 *
 * Isto **não** dispensa a trava entre processos (`NSFileCoordinator` no iOS); reduz o estrago
 * de quando ela falhar. Quem fecha o caso é o caminho de volta ao PIN.
 *
 * # Safety
 *
 * `a_json` e `b_json` precisam ser nulos ou strings válidas; `buf` precisa ser nulo ou ter
 * `cap` bytes.
 */
intptr_t quall_known_peers_merge(const char *a_json,
                                 const char *b_json,
                                 char *buf,
                                 uintptr_t cap);

/**
 * **Instala um gancho de pânico**, para que um pânico do Rust não chegue mudo à casca.
 *
 * # Por que isto existe (dívida 11)
 *
 * O workspace usa `panic = "abort"` e `strip`. Sem gancho, um pânico do núcleo chega ao Android
 * como um `SIGABRT` sem mensagem e ao iOS como uma extension que simplesmente sumiu — e
 * diagnosticar isso custou caro no M3. O `std::panic::set_hook` **ainda roda** antes do abort:
 * a ocorrência, o arquivo e a linha sobrevivem se alguém os escrever em algum lugar.
 *
 * # O que ele faz
 *
 * - Chama `cb` com ocorrência e origem (arquivo sem caminho, linha, coluna), se a casca deu uma.
 *   O payload livre do pânico é omitido em todos os builds: pode conter segredo ou texto do par.
 *   É o caminho recomendado: só a
 *   casca sabe para onde o log dela vai (`NSLog`, `os_log`, o arquivo de diário do iOS).
 * - No **Android**, escreve também no `logcat` com a etiqueta `quall`, via `__android_log_write`
 *   do `liblog` — que o `CMakeLists.txt` da casca já linka. Assim `adb logcat -s quall` mostra
 *   o pânico sem a casca precisar de código nenhum.
 * - Nas demais plataformas, escreve no `stderr`.
 *
 * Chame **uma vez**, o mais cedo possível: do `JNI_OnLoad` no Android, do arranque do app ou da
 * extension no Apple. Chamar de novo substitui o gancho anterior.
 *
 * `cb` nulo instala só o caminho padrão (logcat/stderr), o que já é melhor que o silêncio.
 *
 * # Safety
 *
 * `cb` precisa ser uma função válida ou nulo, e `user_data` precisa continuar vivo pelo resto
 * do processo — o gancho pode disparar a qualquer momento, de qualquer thread.
 */
void quall_install_panic_hook(QuallPanicCallback cb, void *user_data);

/**
 * Gera um PIN de seis dígitos com o gerador do sistema. Padrão `(buf, cap)`.
 *
 * Existe aqui, e não na casca, porque a qualidade do sorteio é o que segura o pareamento: um
 * PIN de seis dígitos tirado de `rand()` com semente de relógio é adivinhável.
 *
 * # Safety
 *
 * `buf` precisa ser nulo ou ter `cap` bytes graváveis.
 */
intptr_t quall_generate_pin(char *buf, uintptr_t cap);

/**
 * Anuncia por mDNS **com um papel**: `"teleprompter"` põe a chave TXT `pa` no anúncio e o papel
 * no nome da instância. `NULL` ou `""` é exatamente [`quall_advertiser_start`].
 *
 * # Safety
 *
 * `me` precisa apontar para um [`QuallDeviceDesc`] válido; `role` precisa ser nulo ou uma string
 * C válida.
 */
struct QuallAdvertiser *quall_advertiser_start_with_role(const struct QuallDeviceDesc *me,
                                                         uint16_t signaling_port,
                                                         const char *role);

/**
 * **Hospeda como teleprompter.** `role` = `"teleprompter"`; `NULL` ou `""` é exatamente
 * [`quall_host_cancelable`].
 *
 * Com o papel, e só com ele:
 *
 * - quem conecta tem de ser um `"controle_remoto"`: qualquer outro é recusado **antes do PIN**,
 *   com motivo legível, e **a espera continua** com o mesmo PIN;
 * - o canal de dados nasce **confiável e sem ordem** (quem hospeda decide; quem conecta adota);
 * - a sessão cai em 5 s de silêncio (as duas pontas mandam estado a cada segundo);
 * - depois que a sessão sobe, a porta continua **atendida**: todo controle que bate ouve
 *   `QUALL_STATUS_BUSY` — inclusive o da sessão, voltando de uma queda que o prompter ainda não
 *   percebeu —, e **nada que chega pela porta derruba esta sessão** (o `Hello` não prova quem é;
 *   quem decide é o detector de 5 s, que faz `quall_session_next_event` devolver
 *   `QUALL_SESSION_EVENT_DISCONNECTED`);
 * - `track_count` tem de ser 0.
 *
 * Depois da queda: bombeada final com `timeout_ms = 0`, `quall_teleprompter_peer_lost`,
 * **`quall_session_close` antes** (é ele que solta a porta; hospedar com a sessão velha de pé
 * falha no `bind`), e hospede de novo **na mesma porta e com o mesmo PIN**. O mesmo PIN só
 * depois da queda de uma sessão que subiu: depois de `QUALL_STATUS_WRONG_PIN` ou
 * `QUALL_STATUS_PAIRING` numa espera, troque o PIN (`quall_generate_pin`) — repeti-lo abriria
 * força bruta. `docs/contrato-teleprompter.md` §2.
 *
 * O prazo (`timeout_ms`) vale para a espera: um prompter que espera o controle por muito tempo
 * passa um prazo longo, ou hospeda de novo com o mesmo PIN quando ele estoura sem erro de PIN.
 *
 * # Safety
 *
 * As mesmas de [`quall_host_cancelable`]; `role` precisa ser nulo ou uma string C válida.
 */
struct QuallSession *quall_host_with_role(const struct QuallSessionOptions *opcoes,
                                          const struct QuallCanceller *cancelador,
                                          const char *role);

/**
 * **Conecta como controle remoto de um teleprompter.** `role` = `"controle_remoto"`; `NULL` ou
 * `""` é exatamente [`quall_connect_cancelable`].
 *
 * **O endereço sem porta ganha a do teleprompter, 7979** ([`quall_teleprompter_default_port`]),
 * e não a 7877 do espelhamento. Um link `quall://` é `QUALL_STATUS_INVALID`: leia-o com
 * [`quall_parse_endpoint_json`] (`docs/contrato-teleprompter.md` §11.1).
 *
 * Diante de um aparelho que não é teleprompter, sai com `QUALL_STATUS_PROTOCOL` e o motivo — e
 * sai com um `Bye`, que para o outro lado é candidato que desistiu: a espera dele continua.
 * Diante de um teleprompter que já tem controle: `QUALL_STATUS_BUSY`, "tente de novo".
 *
 * # Safety
 *
 * As mesmas de [`quall_connect_cancelable`]; `role` precisa ser nulo ou uma string C válida.
 */
struct QuallSession *quall_connect_with_role(const char *endpoint,
                                             const struct QuallSessionOptions *opcoes,
                                             const struct QuallCanceller *cancelador,
                                             const char *role);

/**
 * **Hospeda aceitando também o código de 4 dígitos** (`docs/conexao.md` §4). `role` como em
 * [`quall_host_with_role`]; `confirm` de [`quall_code_confirm_new`] — nulo é exatamente
 * [`quall_host_with_role`].
 *
 * Com `confirm`, quem conecta sem PIN e sem pareamento guardado pode parear tocando Confirmar dos
 * dois lados: a tela daqui mostra o que [`quall_code_confirm_state_json`] disser. **O PIN de
 * `opcoes` continua valendo ao mesmo tempo** — é o caminho do plugin de OBS publicado e de toda
 * casca que ainda não conhece o código. Um código recusado (de qualquer lado) **não** encerra a
 * espera; o terceiro encerra a oferta do código nesta espera.
 *
 * Anuncie com [`quall_advertiser_start_with_code`] e `accepts_code = true` para a lista dos
 * outros mostrar "Conectar" em vez do campo do PIN.
 *
 * # Safety
 *
 * As mesmas de [`quall_host_with_role`]; `confirm` precisa ser nulo ou vir de
 * [`quall_code_confirm_new`] e continuar vivo durante a chamada.
 */
struct QuallSession *quall_host_with_code(const struct QuallSessionOptions *opcoes,
                                          const struct QuallCanceller *cancelador,
                                          const char *role,
                                          const struct QuallCodeConfirm *confirm);

/**
 * **Conecta podendo parear pelo código de 4 dígitos** (`docs/conexao.md` §4). `role` como em
 * [`quall_connect_with_role`]; `width_px`/`height_px` como em [`quall_connect_with_screen`] (0 e 0
 * é "não digo"); `confirm` nulo é o `connect` de sempre.
 *
 * A ordem: PIN das opções, se houver; senão o pareamento guardado (retomada); senão o código —
 * se o outro lado disse que aceita. Diante de um aparelho que não aceita código, sem PIN e sem
 * pareamento, volta logo com `QUALL_STATUS_NEEDS_PIN`, sem ter mandado quadro nenhum: mostre o
 * campo do PIN (a espera do outro lado continua, com o PIN dele na tela).
 *
 * # Safety
 *
 * As mesmas de [`quall_connect_with_role`]; `confirm` precisa ser nulo ou vir de
 * [`quall_code_confirm_new`] e continuar vivo durante a chamada.
 */
struct QuallSession *quall_connect_with_code(const char *endpoint,
                                             const struct QuallSessionOptions *opcoes,
                                             const struct QuallCanceller *cancelador,
                                             const char *role,
                                             const struct QuallCodeConfirm *confirm,
                                             uint32_t width_px,
                                             uint32_t height_px);

/**
 * **O toque em Conectar** (`docs/v3-no-privado.md` §8.6): [`quall_connect_with_code`] sabendo
 * **quem** a pessoa tocou (`expected_device_id`: o item da lista com `known_device_id`, ou a sessão
 * salva tocada). É a regra única das cascas para o toque; a reconexão automática continua em
 * [`quall_connect_known`].
 *
 * - Com vínculo v3 com esse aparelho em `known_peers_json`: **reconecta** em `endpoint` (a rota de
 *   agora), só com o vínculo dele, sem código — mostre "Reconectando…". Se o outro lado não
 *   reconhece o pareamento e há `confirm`, segue **no mesmo toque** para o código, para parear de
 *   novo: o estado do confirmador ganha `"notice": "previous_pairing_invalid"` (diga "O pareamento
 *   anterior não vale mais. Confirme o código para parear de novo."), e o vínculo velho só é trocado
 *   quando a sessão sobe e a casca grava os pares. Sem `confirm`, falha fechado com
 *   `PAIR_NOT_RECOGNIZED` (A2). Uma queda de rede na retomada **não** vira pareamento novo: o núcleo
 *   procura o aparelho de novo (o endereço e a busca) até `opcoes.timeout_ms` e devolve o erro de
 *   rede (`TIMEOUT`, `IO`, `NO_ROUTE`…) se ninguém aparecer.
 * - Sem vínculo: é [`quall_connect_with_code`] (retomada se alguma dica casar, senão o código).
 * - Com PIN em `opcoes`, o PIN pareia de novo, como sempre.
 *
 * Nulo/vazio em `expected_device_id` é [`quall_connect_with_code`].
 *
 * # Safety
 *
 * As de [`quall_connect_with_code`]; `expected_device_id` nulo ou terminado em zero.
 */
struct QuallSession *quall_connect_with_code_to(const char *endpoint,
                                                const struct QuallSessionOptions *opcoes,
                                                const struct QuallCanceller *cancelador,
                                                const char *role,
                                                const struct QuallCodeConfirm *confirm,
                                                uint32_t width_px,
                                                uint32_t height_px,
                                                const char *expected_device_id);

/**
 * **A sessão salva, sem id no anúncio** (`docs/v3-no-privado.md` §5.2): acha o aparelho
 * `device_id` — no endereço antigo `last_endpoint` (pode ser nulo) e nos anfitriões da busca
 * (mDNS, broadcast, sondagem da /24) — e **retoma com ele na mesma conexão**, só com o vínculo
 * dele (os outros pares de `known_peers_json` não entram). Nunca usa o código nem o PIN de
 * `opcoes`. Bloqueia até `opcoes.timeout_ms` (0 = o padrão).
 *
 * Devolve a sessão como [`quall_connect`]; o endereço em que conectou e por onde o achou saem
 * em [`quall_session_endpoint_json`] — grave a sessão salva com eles **depois** desta chamada
 * voltar (a retomada é o que autentica). Nulo: `NEEDS_PIN` (não há vínculo v3 com o aparelho:
 * pareie de novo), `PAIR_NOT_RECOGNIZED` (o endereço antigo, ou o único candidato que serve ao
 * papel, respondeu sem reconhecer este pareamento: ele esqueceu este aparelho — ofereça "Esquecer"
 * e parear de novo), `TIMEOUT` (ninguém com a dica dele respondeu: outra rede, desligado, ou vários
 * aparelhos sem a dica), `CANCELLED`, `INVALID`.
 *
 * `role`: `NULL`/`""` para vídeo, `"controle_remoto"` para o controle de um teleprompter — o
 * controle reconecta ao prompter guardado por aqui (a porta que falta é a 7979).
 *
 * # Safety
 *
 * `device_id` terminado em zero; `last_endpoint` nulo ou terminado em zero; `opcoes` válido;
 * `cancelador` nulo ou vivo durante a chamada.
 */
struct QuallSession *quall_connect_known(const char *device_id,
                                         const char *last_endpoint,
                                         const struct QuallSessionOptions *opcoes,
                                         const struct QuallCanceller *cancelador,
                                         const char *role);

/**
 * [`quall_connect_known`] **com a tela do receptor** (como [`quall_connect_with_screen`]): o
 * transmissor ajusta a captura ao tamanho dela. `0, 0` é exatamente [`quall_connect_known`]; um
 * lado fora de `1..=Screen::LADO_MAXIMO` é `INVALID`.
 *
 * # Safety
 *
 * As mesmas de [`quall_connect_known`].
 */
struct QuallSession *quall_connect_known_with_screen(const char *device_id,
                                                     const char *last_endpoint,
                                                     const struct QuallSessionOptions *opcoes,
                                                     const struct QuallCanceller *cancelador,
                                                     const char *role,
                                                     uint32_t width_px,
                                                     uint32_t height_px);

/**
 * Onde a sessão do **convidado** conectou: `{"endpoint":"192.168.15.9:7877","via":"udp"}`. `via`
 * só vem de [`quall_connect_known`] (`"endereco_antigo"`, `"mdns"`, `"udp"`, `"sondagem"`); nas
 * outras é `null`. No anfitrião, `endpoint` é `null`. Padrão `(buf, cap)`.
 *
 * # Safety
 *
 * `s` precisa vir de [`quall_host`] ou [`quall_connect`] e estar viva; `buf` nulo ou com `cap` bytes.
 */
intptr_t quall_session_endpoint_json(const struct QuallSession *s,
                                     char *buf,
                                     uintptr_t cap);

/**
 * **A porta do teleprompter**: 7979. O prompter hospeda nela ([`quall_teleprompter_pick_port`]), e
 * [`quall_connect_with_role`] a usa para completar o endereço sem porta do controle. Uma função, e
 * não um `#define`, pelo motivo de [`quall_protocol_version`] (`docs/contrato-teleprompter.md`
 * §11.1).
 */
uint16_t quall_teleprompter_default_port(void);

/**
 * **A porta em que o prompter vai hospedar**, escolhida **uma vez, ao abrir a tela**: a 7979,
 * esperando por ela até `wait_ms` (2 000 é o recomendado: numa recriação da tela, a sessão velha
 * ainda a segura por um instante); senão a primeira livre de 7980 a 7988; senão uma efêmera. `0`
 * só se nem isso.
 *
 * **A volta depois de uma queda é sempre na mesma porta** — o controle que caiu tenta de novo no
 * endereço que tinha. Não chame isto de novo a cada sessão. (O nome não é `_free_port` porque, no
 * `quall.h`, `_free` é destrutor.)
 */
uint16_t quall_teleprompter_pick_port(uint32_t wait_ms);

/**
 * **Lê um endereço ou um link `quall://<pin>@<host>:<porta>`** — o que a pessoa digitou, colou, ou
 * o QR trouxe —, sem rede nenhuma. Padrão `(buf, cap)`:
 *
 * ```json
 * {"endereco":"192.168.15.8:7979","pin":"424242"}
 * ```
 *
 * `"pin"` é `null` quando a entrada não era um link. Sem porta, a do papel: `role`
 * `"controle_remoto"` → 7979; nulo ou `""` → 7877 (vídeo); outro papel é `QUALL_STATUS_INVALID`. O
 * que não se entende — PIN do link que não tem exatamente seis dígitos, link sem `@`, espaço no
 * meio, porta 0 — é `-1` com `QUALL_STATUS_INVALID` e o motivo em `quall_last_error()`.
 *
 * **A regra da casca** (`docs/contrato-teleprompter.md` §11.1): ler um link preenche o endereço e
 * o PIN nos campos; conectar manda só o endereço (o PIN vai nas opções); a volta automática depois
 * de uma queda vai com o endereço e **sem** PIN. `quall_connect*` recusa link.
 *
 * # Safety
 *
 * `text` precisa ser uma string C válida; `role`, nulo ou uma string C válida; `buf`, nulo ou com
 * `cap` bytes.
 */
intptr_t quall_parse_endpoint_json(const char *text,
                                   const char *role,
                                   char *buf,
                                   uintptr_t cap);

/**
 * **O teto de uma mensagem**, em bytes: 262 144 (256 KiB). Uma função, e não um `#define`, pelo
 * motivo de [`quall_protocol_version`]: a casca Kotlin copiaria o número à mão e nada conferiria.
 */
uintptr_t quall_message_max_bytes(void);

/**
 * **O handle das mensagens da sessão**: mandar e receber texto pelo canal de dados.
 *
 * É do chamador: libere com [`quall_messages_free`]. **Sobrevive a [`quall_session_close`]**,
 * como o de track: depois do fechamento, mandar devolve `QUALL_STATUS_CLOSED` sem tocar na
 * biblioteca, e ler entrega o que já tinha chegado e depois `QUALL_STATUS_CLOSED`. Pode ser pedido
 * mais de uma vez; todos os handles de uma sessão dividem a mesma fila.
 *
 * # Safety
 *
 * `s` precisa vir de [`quall_host`] ou [`quall_connect`] (ou das variantes) e estar viva.
 */
struct QuallMessages *quall_session_messages(const struct QuallSession *s);

/**
 * **Manda uma mensagem**: UTF-8, terminada em NUL, de 1 a [`quall_message_max_bytes`] bytes.
 * **Pode ser chamada de qualquer thread**, inclusive da thread da interface: não bloqueia.
 *
 * - vazia ou acima do teto: `QUALL_STATUS_INVALID`, e nada sai (o teto é conferido **antes** da
 *   biblioteca, que lançaria exceção);
 * - o canal ainda não abriu: `QUALL_STATUS_TRANSPORT` — tente de novo, não é queda;
 * - a sessão fechou, ou o canal fechou: `QUALL_STATUS_CLOSED`.
 *
 * Numa sessão de vídeo o canal é **sem retransmissão**: uma mensagem maior que um pedaço SCTP
 * (~1,1 KB) não tem garantia nenhuma de chegar. Numa sessão de teleprompter é confiável.
 *
 * # Safety
 *
 * `m` precisa vir de [`quall_session_messages`]; `message` precisa ser uma string C válida.
 */
enum QuallStatus quall_messages_send(const struct QuallMessages *m, const char *message);

/**
 * **A próxima mensagem**, esperando até `timeout_ms`, no padrão `(buf, cap)` — com uma diferença
 * que importa: **a mensagem só sai da fila quando coube**.
 *
 * | devolve | quer dizer |
 * |---|---|
 * | `0` | nada chegou no prazo |
 * | `n > 0` e `n <= cap` | a mensagem foi escrita em `buf` (com o NUL) **e consumida** |
 * | `n > cap` (ou `buf` nulo) | há uma mensagem de `n` bytes com o NUL, **não consumida**: aloque `n` e chame de novo (com `timeout_ms = 0`) |
 * | negativo | erro; o motivo em [`quall_last_status`]: `QUALL_STATUS_CLOSED` quando a sessão acabou e a fila esvaziou |
 *
 * Uma mensagem nunca é vazia (a vazia, a com NUL e a que não é UTF-8 são descartadas na chegada
 * e contadas), então `0` não se confunde com mensagem. `buf` nulo com `cap > 0` é
 * `QUALL_STATUS_NULL_POINTER`.
 *
 * **Avança estado: uma thread só.** E numa sessão de teleprompter quem lê é
 * [`quall_teleprompter_pump`] — as duas funções leem a mesma fila, e cada mensagem vai para quem
 * ler primeiro.
 *
 * # Safety
 *
 * `m` precisa vir de [`quall_session_messages`]; `buf` precisa ser nulo ou ter `cap` bytes.
 */
intptr_t quall_messages_next(const struct QuallMessages *m,
                             uint32_t timeout_ms,
                             char *buf,
                             uintptr_t cap);

/**
 * Os contadores das mensagens da sessão, como JSON. Padrão `(buf, cap)`:
 *
 * ```json
 * {"enviadas":0,"recebidas":0,"descartadas_fila_cheia":0,"descartadas_invalidas":0}
 * ```
 *
 * # Safety
 *
 * `m` precisa vir de [`quall_session_messages`]; `buf` precisa ser nulo ou ter `cap` bytes.
 */
intptr_t quall_messages_stats_json(const struct QuallMessages *m, char *buf, uintptr_t cap);

/**
 * Libera o handle das mensagens. Nulo é ignorado. Pode ser antes ou depois de
 * [`quall_session_close`].
 *
 * # Safety
 *
 * `m` precisa vir de [`quall_session_messages`] e não pode ter sido liberado antes.
 */
void quall_messages_free(struct QuallMessages *m);

/**
 * **O mensageiro da câmera desta sessão** (`docs/controle-remoto-da-camera.md` §17.2): manda pelo
 * mesmo canal que `m`, e lê de uma **caixa própria**, para onde vão as mensagens da câmera
 * (`"app":"camera"`) que chegam à sessão. É como o controle remoto da câmera (R9b) passa pela
 * sessão do **teleprompter**: o prompter R5 bombeia o seu `QuallCameraHost` com ele, e o controle o
 * seu `QuallCameraRemote`, enquanto `quall_teleprompter_pump` lê o handle comum e nunca vê a câmera.
 *
 * Na primeira vez **liga a triagem** da sessão: dali em diante, a câmera só chega por um handle
 * destes. Pedido de novo (de `m` ou de um handle da câmera), é outro handle da mesma caixa.
 * **Numa sessão de vídeo, não peça**: lá o leitor da câmera é o handle comum. É do chamador: libere
 * com [`quall_messages_free`]; sobrevive a [`quall_session_close`] como o comum, e depois do fim lê
 * `QUALL_STATUS_CLOSED`. `NULL` com `m` nulo.
 *
 * # Safety
 *
 * `m` precisa vir de [`quall_session_messages`] (ou desta função) e estar vivo.
 */
struct QuallMessages *quall_messages_for_camera(const struct QuallMessages *m);

/**
 * **O teto do roteiro**, em bytes de UTF-8: 131 072.
 */
uintptr_t quall_teleprompter_max_text_bytes(void);

/**
 * **Cria a réplica deste aparelho.** Uma por aparelho, e ela vive mais que a sessão: guarde-a
 * enquanto o app estiver aberto, e passe a mesma a cada sessão nova.
 *
 * - `author_id`: o `device_id` deste aparelho (1 a 256 bytes) — desempata edições no mesmo
 *   milissegundo;
 * - `role`: `"teleprompter"` (mostra o texto) ou `"controle_remoto"`. Ao começar uma sessão nova,
 *   o controle zera `rolando`, `posicao` e `salto` e adota os do prompter;
 * - `saved_json`: o que [`quall_teleprompter_saved_json`] devolveu numa vida anterior, ou nulo.
 *   JSON ilegível devolve nulo com `QUALL_STATUS_INVALID`: chame de novo com nulo.
 *
 * Libere com [`quall_teleprompter_free`].
 *
 * # Safety
 *
 * As três strings precisam ser nulas (só `saved_json` pode) ou strings C válidas.
 */
struct QuallTeleprompter *quall_teleprompter_new(const char *author_id,
                                                 const char *role,
                                                 const char *saved_json);

/**
 * Libera a réplica. Nulo é ignorado. Guarde antes o [`quall_teleprompter_saved_json`].
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`] e não pode ter sido liberado antes, nem estar em
 * uso em outra thread.
 */
void quall_teleprompter_free(struct QuallTeleprompter *t);

/**
 * Troca o roteiro. **Chame ao confirmar a edição, nunca a cada tecla.** Acima de
 * [`quall_teleprompter_max_text_bytes`], com NUL, ou se o texto escapado não couber numa
 * mensagem: `QUALL_STATUS_INVALID`, e o texto anterior fica. Qualquer thread.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`]; `text` precisa ser uma string C válida.
 */
enum QuallStatus quall_teleprompter_set_text(const struct QuallTeleprompter *t, const char *text);

/**
 * Rola ou para. Qualquer thread; a mudança sai na hora.
 *
 * Com o "segurar para rolar" (§12.3): `false` **sempre** para e sai do segurar; `true` só faz
 * alguma coisa com o texto parado — com o texto rolando pelo dedo no botão, não muda nada, e
 * [`quall_teleprompter_release`] e a queda seguem parando.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_set_scrolling(const struct QuallTeleprompter *t,
                                                  bool scrolling);

/**
 * Velocidade em linhas por segundo (linha = altura da linha na fonte do prompter), de 0,05 a 20,
 * em centésimos. Fora da faixa ou NaN: `QUALL_STATUS_INVALID`.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_set_speed(const struct QuallTeleprompter *t,
                                              double lines_per_second);

/**
 * Fonte em pontos lógicos (pt no iOS e no Mac, sp no Android, DIP no Windows), de 8 a 400.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_set_font_size(const struct QuallTeleprompter *t, double points);

/**
 * Margem, fração da largura da vista do texto, de cada lado, de 0 a 0,45.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_set_margin(const struct QuallTeleprompter *t, double fraction);

/**
 * Linha de leitura, fração da altura da vista do texto a partir do topo, de 0 a 1.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_set_reading_line(const struct QuallTeleprompter *t,
                                                     double fraction);

/**
 * Espelho: inverte a **vista do texto** na horizontal (não o vídeo).
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_set_mirror(const struct QuallTeleprompter *t, bool mirror);

/**
 * **O relato de posição de quem mostra o texto**, como fração do percurso (0 = começo na linha de
 * leitura, 1 = fim nela). Pode ser chamado a cada quadro: o envio é limitado a 4 Hz e sai na
 * bombeada. **Só o prompter**: no controle é `QUALL_STATUS_INVALID`.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_set_position(const struct QuallTeleprompter *t,
                                                 double fraction);

/**
 * **Salta** para `fraction` (0 a 1). "Voltar ao começo" é `jump(0)`; duas vezes são dois saltos.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_jump(const struct QuallTeleprompter *t, double fraction);

/**
 * **Pula** `delta` (de -1 a 1) a partir de onde o texto **vai estar**: o último salto daqui, se o
 * relato ainda não passou dele, senão a posição relatada. Dois toques rápidos são dois pulos.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_jump_by(const struct QuallTeleprompter *t, double delta);

/**
 * **A bombeada.** Manda o que está devido (o batimento de 1 s, o relato de posição, o texto quando
 * o outro lado não o tem), espera até `timeout_ms` por mensagem, funde o que chegou, e escreve em
 * `changed` (que pode ser nulo) os bits de [`QuallTeleprompterChange`] do que mudou **por causa do
 * outro lado**.
 *
 * Chame em laço, da thread da sessão, com `timeout_ms` de no máximo 250 (50 a 100 é o
 * recomendado), junto com `quall_session_next_event(s, 0)` — é este que diz que o outro lado
 * saiu. As edições da tela **não** esperam a bombeada: saem na hora, da thread de quem edita.
 *
 * # `QUALL_STATUS_CLOSED` vem **com** `changed` preenchido
 *
 * Quer dizer que a sessão acabou — **e a fila já foi lida até o fim**: a última mensagem do outro
 * lado (a pausa que o controle tocou antes de cair) foi fundida, e o bit dela está em `changed`.
 * **Aplique `changed` antes de qualquer outra coisa** e só então chame
 * [`quall_teleprompter_peer_lost`]. Uma falha de envio nunca impede a leitura nem apaga o que foi
 * fundido. (Até a revisão de 13/09, `CLOSED` vinha com `changed = 0` e a pausa se perdia: a
 * réplica dizia parado e a tela seguia rolando.)
 *
 * Quando a queda chega por `quall_session_next_event` (`DISCONNECTED`) e não pela bombeada, faça
 * **uma bombeada final com `timeout_ms = 0`**, aplique o `changed` dela, e só então chame
 * `quall_teleprompter_peer_lost`.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`], `m` de [`quall_session_messages`]; `changed`
 * precisa ser nulo ou apontar para um `uint32_t`.
 */
enum QuallStatus quall_teleprompter_pump(const struct QuallTeleprompter *t,
                                         const struct QuallMessages *m,
                                         uint32_t timeout_ms,
                                         uint32_t *changed);

/**
 * **O outro lado sumiu** (a sessão caiu). Aplica a regra de queda — decidida pelo usuário: o
 * prompter **continua no estado em que estava**, rolando segue rolando, parado segue parado, e as
 * duas telas mostram um aviso até o controle voltar (`par_visto_ha_ms` fica nulo) —, esquece o que
 * sabia do par, e escreve em `changed` o que mudou (sempre com `QUALL_TELEPROMPTER_CHANGE_PEER`).
 *
 * **A exceção é o "segurar para rolar"** (`docs/contrato-teleprompter.md` §12.4, decisão do
 * usuário de 14/09): com `"segurando"`, **o texto para**, nos dois lados, como se a pessoa tivesse
 * soltado, e `changed` traz `QUALL_TELEPROMPTER_CHANGE_SCROLLING` e `QUALL_TELEPROMPTER_CHANGE_HOLD`.
 * No controle, essa parada é só da réplica dele e não vai ao fio: o prompter para sozinho, pela
 * queda dele, por 2,5 s sem ouvir o controle ou — se a casca dele não chamar `peer_lost` — ao
 * começar a sessão seguinte, também só na réplica dele.
 *
 * Chame em **todo** fim de sessão, antes de qualquer outra edição (a ordem do fim, §6).
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`]; `changed` precisa ser nulo ou apontar para um
 * `uint32_t`.
 */
enum QuallStatus quall_teleprompter_peer_lost(const struct QuallTeleprompter *t,
                                              uint32_t *changed);

/**
 * **O estado para a tela desenhar**, como JSON, sem o texto. Padrão `(buf, cap)` — e o conteúdo
 * pode mudar entre a chamada que pergunta o tamanho e a que escreve: repita até caber.
 *
 * ```json
 * {"rolando":false,"velocidade":1.0,"fonte":48.0,"margem":0.1,"linha_de_leitura":0.3,
 *  "espelho":false,"posicao":0.0,"salto":null,"texto_bytes":0,"par_visto_ha_ms":null,
 *  "sem_confirmacao_ha_ms":null,"contadores":{"estados_enviados":0,"textos_enviados":0,
 *  "recebidas":0,"invalidas":0,"de_outro_app":0,"de_outra_versao":0,"campos_recusados":0,
 *  "carimbos_do_futuro":0,"mensagens_impossiveis":0,"reenvios_desistidos":0},
 *  "pergunta_do_texto":null,"copias_do_texto":[],"para_tras":false,"segurando":false,
 *  "par_entende_segurar":false,"gravando_ha_ms":null,"pedido_de_gravacao":null,
 *  "gravacao_recusada":null,"par_entende_gravar":false}
 * ```
 *
 * A gravação (`docs/contrato-teleprompter.md` §13): `"gravando_ha_ms"` é a duração que o prompter
 * relata (`null` sem gravar); `"pedido_de_gravacao"` é `{"n":…,"gravar":true,"ha_ms":…}` — no
 * prompter, o pedido que a casca decide; no controle, o daqui sem resposta; `"gravacao_recusada"` é
 * `{"n":…,"gravar":true,"motivo":"…"}`.
 *
 * Com a pergunta do texto aberta (`docs/contrato-teleprompter.md` §11.6):
 *
 * ```json
 * "pergunta_do_texto":{"aberta":true,"retido_ha_ms":840,"prompter_id":"ipad-a1b2",
 *  "prompter_nome":"iPad da Maria","meu":{"bytes":10,"resumo":"…","previa":"Boa noite."},
 *  "do_prompter":{"bytes":15,"resumo":"…","previa":"Bom dia a todos"}}
 * ```
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`]; `buf` precisa ser nulo ou ter `cap` bytes.
 */
intptr_t quall_teleprompter_state_json(const struct QuallTeleprompter *t,
                                       char *buf,
                                       uintptr_t cap);

/**
 * **O roteiro.** Padrão `(buf, cap)`; o texto pode mudar entre as duas chamadas (chegou uma
 * edição do outro lado): repita até caber.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`]; `buf` precisa ser nulo ou ter `cap` bytes.
 */
intptr_t quall_teleprompter_text(const struct QuallTeleprompter *t, char *buf, uintptr_t cap);

/**
 * **O que guardar entre vidas do app**: os seis campos que persistem, com os carimbos. Até
 * ~260 KiB. Guarde quando o app for para o segundo plano e ao fechar a sessão, e devolva a
 * [`quall_teleprompter_new`]. Padrão `(buf, cap)`.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`]; `buf` precisa ser nulo ou ter `cap` bytes.
 */
intptr_t quall_teleprompter_saved_json(const struct QuallTeleprompter *t, char *buf, uintptr_t cap);

/**
 * **Liga a pergunta do texto** (`docs/contrato-teleprompter.md` §11.10). Chame logo depois de
 * [`quall_teleprompter_new`], **só quando a casca tiver a tela da pergunta** (a caixa "usar o do
 * prompter / mandar o meu" e [`quall_teleprompter_resolve_text`]). Ligada no meio de uma sessão,
 * vale a partir da seguinte.
 *
 * **Desligada (o padrão)**, o texto nunca é retido: vale "o último que mudou", como sempre, byte a
 * byte no fio; `"pergunta_do_texto"` é sempre `null`. O que sobra são as cópias da fusão — o texto
 * que perde no primeiro encontro com um prompter, dos dois lados, e o texto daqui que mudou desde a
 * última convergência e perde no reencontro (§11.5). No prompter, não muda nada.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_enable_text_question(const struct QuallTeleprompter *t);

/**
 * **Diz que a tela deste prompter entende o "segurar para rolar"** (§12): com `"rolando"` e
 * `"para_tras"`, ela rola para trás na velocidade de sempre e para no começo; e para quando
 * `"rolando"` cai. Chame ao abrir a tela do prompter, **só se ela faz isso**: o estado passa a levar
 * `"entende_segurar": true`, e só então um controle segura. No controle, não muda nada.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_enable_hold(const struct QuallTeleprompter *t);

/**
 * **Aperta o botão do "segurar para rolar"** (§12): `rolando`, `para_tras` (`backwards`) e
 * `segurando` numa mensagem só, com um carimbo só, que sai na hora. Enquanto segura, o prompter rola
 * na velocidade de sempre; [`quall_teleprompter_release`] para. **Se a sessão cair com o dedo no
 * botão, o texto para** (decisão do usuário, 14/09) — e também depois de 2,5 s sem ouvir o outro
 * lado.
 *
 * - `QUALL_STATUS_PROTOCOL`: o prompter não diz que entende (`"par_entende_segurar": false` no
 *   estado) — um prompter de 13/09, ou uma tela que não liga o segurar, rolaria para a frente quando
 *   se pede para trás, e seguiria rolando na queda. Mostre o modo desligado;
 * - `QUALL_STATUS_CLOSED`: sem sessão — e também **depois de a bombeada devolver
 *   `QUALL_STATUS_CLOSED`**, antes mesmo de [`quall_teleprompter_peer_lost`]: o aperto nunca vai
 *   para o prompter da sessão seguinte;
 * - `QUALL_STATUS_INVALID`: numa réplica de prompter.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_hold(const struct QuallTeleprompter *t,
                                         bool backwards);

/**
 * **Solta o botão**: o texto para, numa mensagem só. Sem nada seguro, não faz nada (o segurar pode
 * já ter parado sozinho, na queda ou no silêncio).
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_release(const struct QuallTeleprompter *t);

/**
 * **Diz se a tela deste prompter grava** (`docs/contrato-teleprompter.md` §13): a tela do
 * teleprompter com câmera liga (`enabled = true`) ao abrir e desliga ao fechar. Ligada, o estado leva
 * `"entende_gravar": true`, e só então um controle pede; desligada, um pedido que ainda chegue é
 * recusado pelo núcleo ("o prompter não está na tela que grava"), e o que estava aberto também. Não
 * mexe numa gravação em curso.
 * No controle, não muda nada.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_enable_recording(const struct QuallTeleprompter *t,
                                                     bool enabled);

/**
 * **Diz se a tela deste prompter tem a câmera para o controle controlar** (R9b pelo canal do
 * teleprompter, `docs/controle-remoto-da-camera.md` §17.3): a tela do prompter R5 liga
 * (`enabled = true`) quando bombeia o seu `QuallCameraHost` sobre [`quall_messages_for_camera`] da
 * sessão do teleprompter, e desliga ao fechar. Ligada, o estado leva `"entende_camera": true`, e o
 * controle vê `"par_entende_camera"` (o bit `QUALL_TELEPROMPTER_CHANGE_CAMERA`). No controle, não
 * muda nada.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_enable_camera(const struct QuallTeleprompter *t, bool enabled);

/**
 * **O relato da gravação, por quem grava** (§13): `true` quando o arquivo começou, `false` quando
 * fechou — pelo botão da tela, por um pedido do controle, por falta de espaço, pela câmera perdida.
 * O estado passa a dizer `"gravando_ha_ms"`, contado no relógio monotônico daqui, e o controle o
 * vê como duração, nunca como hora. Sai na hora. Repetir o valor atual não recomeça a contagem.
 *
 * **Aceitar um pedido do controle é chamar isto com o `"gravar"` dele**, mesmo que já esteja assim.
 *
 * - `QUALL_STATUS_INVALID`: numa réplica de controle — o prompter é o único escritor; o controle
 *   pede com [`quall_teleprompter_request_record`] e [`quall_teleprompter_request_stop`].
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_set_recording(const struct QuallTeleprompter *t,
                                                  bool recording);

/**
 * **Recusa o pedido de gravação aberto `n`** (§13), com um motivo legível que o controle mostra
 * como está (`"gravacao_recusada"`): "sem espaço: sobram 312 MB", "a câmera não está entregando".
 * `n` é o `"n"` de `"pedido_de_gravacao"` que a casca leu e decidiu. Sai na hora.
 *
 * - `QUALL_STATUS_BUSY`: o pedido `n` foi substituído por outro enquanto a casca decidia — nada foi
 *   recusado; releia `"pedido_de_gravacao"` e decida o novo;
 * - `QUALL_STATUS_INVALID`: não há pedido aberto; o motivo é vazio ou passa de 256 bytes; ou a
 *   réplica é de controle;
 * - `QUALL_STATUS_NULL_POINTER`: `reason` nulo.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`]; `reason` precisa ser uma string C válida.
 */
enum QuallStatus quall_teleprompter_refuse_recording(const struct QuallTeleprompter *t,
                                                     uint64_t n,
                                                     const char *reason);

/**
 * **O controle pede ao prompter que comece a gravar** (§13). Quem decide é o prompter; a resposta
 * volta pelo estado: `"pedido_de_gravacao"` volta a `null`, e `"gravando_ha_ms"` passa a contar ou
 * `"gravacao_recusada"` diz por quê (bit `QUALL_TELEPROMPTER_CHANGE_RECORDING`). Sai na hora.
 *
 * - `QUALL_STATUS_PROTOCOL`: o prompter não diz que grava (`"par_entende_gravar": false`) — uma
 *   build anterior, ou uma tela que não grava. **Não mostre o botão**;
 * - `QUALL_STATUS_CLOSED`: sem sessão, ou com ela já acabada — um pedido não atravessa sessão;
 * - `QUALL_STATUS_INVALID`: numa réplica de prompter.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_request_record(const struct QuallTeleprompter *t);

/**
 * **O controle pede ao prompter que pare de gravar** (§13). As mesmas regras de
 * [`quall_teleprompter_request_record`].
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`].
 */
enum QuallStatus quall_teleprompter_request_stop(const struct QuallTeleprompter *t);

/**
 * **A escolha da pergunta do texto** (`docs/contrato-teleprompter.md` §11.4): `keep_mine = false`
 * é "usar o do prompter" (o controle adota o registro dele e guarda o daqui como cópia);
 * `keep_mine = true` é "mandar o meu" (o texto daqui é recarimbado acima do dele, sai, e o dele
 * fica guardado). `seen_digest` é o `"resumo"` de `"do_prompter"` que a tela mostrou.
 *
 * - `QUALL_STATUS_OK`: grave o salvo agora ([`quall_teleprompter_saved_json`]) — a cópia só existe
 *   na memória até ser gravada;
 * - `QUALL_STATUS_INVALID`: não há pergunta aberta (ou ainda está comparando);
 * - `QUALL_STATUS_BUSY`: o texto do prompter mudou desde que a pergunta foi mostrada, ou ele tem
 *   um texto mais novo a caminho. Espere `QUALL_TELEPROMPTER_CHANGE_TEXT_QUESTION` e mostre de novo;
 * - `QUALL_STATUS_CLOSED`: o prompter não está conectado — as escolhas ficam desligadas;
 * - `QUALL_STATUS_NULL_POINTER`: `seen_digest` nulo.
 *
 * Qualquer thread; sai na hora, como os `set_*`.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`]; `seen_digest` precisa ser uma string C válida.
 */
enum QuallStatus quall_teleprompter_resolve_text(const struct QuallTeleprompter *t,
                                                 bool keep_mine,
                                                 const char *seen_digest);

/**
 * **O texto do prompter na pergunta aberta**, no padrão `(buf, cap)` — repita até caber. Sem
 * pergunta aberta (inclusive comparando): `-1` com `QUALL_STATUS_INVALID`. O texto **deste**
 * controle continua em [`quall_teleprompter_text`]: retido, a réplica mostra o dele.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`]; `buf` precisa ser nulo ou ter `cap` bytes.
 */
intptr_t quall_teleprompter_question_text(const struct QuallTeleprompter *t,
                                          char *buf,
                                          uintptr_t cap);

/**
 * **O texto inteiro de uma cópia**, pelo `"resumo"` que `"copias_do_texto"` mostra. Padrão
 * `(buf, cap)`. Um resumo que não está na lista é `-1` com `QUALL_STATUS_INVALID`. "Usar este" é
 * um [`quall_teleprompter_set_text`] comum com ele.
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`]; `digest` precisa ser uma string C válida; `buf`,
 * nulo ou com `cap` bytes.
 */
intptr_t quall_teleprompter_text_copy(const struct QuallTeleprompter *t,
                                      const char *digest,
                                      char *buf,
                                      uintptr_t cap);

/**
 * **Apaga uma cópia**, pelo `"resumo"`. `QUALL_STATUS_INVALID` se não havia. Grave o salvo
 * depois (o bit `QUALL_TELEPROMPTER_CHANGE_TEXT_COPY` acende na bombeada seguinte).
 *
 * # Safety
 *
 * `t` precisa vir de [`quall_teleprompter_new`]; `digest` precisa ser uma string C válida.
 */
enum QuallStatus quall_teleprompter_forget_text_copy(const struct QuallTeleprompter *t,
                                                     const char *digest);

/**
 * **Cria o filmador**, sem câmera e **com a permissão desligada** (o padrão do contrato). Chame
 * [`quall_camera_host_set_allowed`] com a opção salva e [`quall_camera_host_set_camera`] com a
 * câmera. Libere com [`quall_camera_host_free`].
 */
struct QuallCameraHost *quall_camera_host_new(void);

/**
 * Libera o filmador. Nulo é aceito.
 *
 * # Safety
 *
 * `h` precisa ser nulo ou vir de [`quall_camera_host_new`], e não ser usado depois.
 */
void quall_camera_host_free(struct QuallCameraHost *h);

/**
 * Liga ou desliga **"Permitir controle remoto da câmera"**. Desligada, os pedidos são recusados
 * com `nao_permitido` e os receptores mostram os controles apagados. Qualquer thread.
 *
 * # Safety
 *
 * `h` precisa vir de [`quall_camera_host_new`].
 */
enum QuallStatus quall_camera_host_set_allowed(const struct QuallCameraHost *h, bool allowed);

/**
 * **A câmera em uso**: as capacidades (contrato §3.2, até 2.048 bytes) e o registro do R9 dela
 * (até 1.024 bytes). **Os dois nulos** = sem câmera (a câmera fechou, a fonte é a tela). Um nulo
 * só: `QUALL_STATUS_INVALID`. Chame a cada abertura e troca de câmera ou de lente: os pedidos
 * em trânsito da câmera anterior são recusados com `camera_trocada`. Para faixas novas da mesma
 * câmera, [`quall_camera_host_set_capabilities`]. Qualquer thread.
 *
 * # Safety
 *
 * `h` precisa vir de [`quall_camera_host_new`]; as strings, nulas ou strings C válidas.
 */
enum QuallStatus quall_camera_host_set_camera(const struct QuallCameraHost *h,
                                              const char *capabilities_json,
                                              const char *settings_json);

/**
 * **As faixas novas da mesma câmera** (o fps que muda o teto do obturador, o degrau de calor do
 * iOS): os pedidos em trânsito continuam valendo, conferidos contra a faixa nova. Sem câmera:
 * `QUALL_STATUS_INVALID`.
 *
 * # Safety
 *
 * `h` precisa vir de [`quall_camera_host_new`]; `capabilities_json`, uma string C válida.
 */
enum QuallStatus quall_camera_host_set_capabilities(const struct QuallCameraHost *h,
                                                    const char *capabilities_json);

/**
 * **O registro que ficou valendo**, inteiro. `request` é `0` para uma mudança feita **no
 * filmador** (o painel, o toque na prévia) ou o `"n"` do pedido que [`quall_camera_host_next_request`]
 * entregou — aí o núcleo dá o recibo ao receptor e mostra "Controlado por" por 4 s
 * (`"controlado_por"` no estado). Um `n` já respondido, vencido (5 s) ou de outra câmera:
 * `QUALL_STATUS_INVALID`, e nada muda. Qualquer thread, mas **toda** mudança do registro passa
 * pela mesma fila do dono da câmera (contrato §6).
 *
 * # Safety
 *
 * `h` precisa vir de [`quall_camera_host_new`]; `settings_json`, uma string C válida.
 */
enum QuallStatus quall_camera_host_set_settings(const struct QuallCameraHost *h,
                                                const char *settings_json,
                                                uint64_t request);

/**
 * **O registro que a casca escreveu sozinha**: o valor lido que a trava guarda, o "Travado de
 * novo depois de medir a cena", o foco lido ao travar (R9 §2.1). Os receptores recebem o
 * registro novo, mas **nenhum campo muda de dono** para o "vence o último", e o `autor` fica.
 *
 * # Safety
 *
 * `h` precisa vir de [`quall_camera_host_new`]; `settings_json`, uma string C válida.
 */
enum QuallStatus quall_camera_host_update_settings(const struct QuallCameraHost *h,
                                                   const char *settings_json);

/**
 * **O que a câmera diz estar usando** (R9 §3.6), até 512 bytes: `iso`, `obturadorNs`, `kelvin`,
 * `abertura`, `focoPosicao`, e `divergentes`. Sai aos receptores no máximo 4 vezes por segundo.
 *
 * # Safety
 *
 * `h` precisa vir de [`quall_camera_host_new`]; `read_json`, uma string C válida.
 */
enum QuallStatus quall_camera_host_set_read(const struct QuallCameraHost *h, const char *read_json);

/**
 * **A casca não conseguiu aplicar o pedido `request`.** `reason` é um código `[a-z0-9_]` de até
 * 32 bytes: `nao_aplicado`, `fora_da_imagem` (o toque caiu numa tarja), ou outro. Nulo:
 * `QUALL_STATUS_NULL_POINTER`.
 *
 * # Safety
 *
 * `h` precisa vir de [`quall_camera_host_new`]; `reason`, uma string C válida.
 */
enum QuallStatus quall_camera_host_reject(const struct QuallCameraHost *h,
                                          uint64_t request,
                                          const char *reason);

/**
 * **O próximo pedido aceito**, como JSON, no padrão `(buf, cap)` — e **só tira da fila quando
 * coube**:
 *
 * ```json
 * {"n":5,"autor":"OBS no Dell","autor_id":"dell-7f2a","ajuste":{"exposicao":"manual","iso":800},
 *  "restaurar":false,"toque":null}
 * ```
 *
 * | devolve | quer dizer |
 * |---|---|
 * | `0` | a fila está vazia |
 * | `n > 0` e `n <= cap` | escrito em `buf` com o NUL, **e tirado da fila** |
 * | `n > cap`, ou `buf` nulo | há um pedido de `n` bytes com o NUL, **ainda na fila**: aloque `n` e chame de novo |
 * | negativo | erro, o código em `quall_last_status()` |
 *
 * Aplique na ordem do contrato §6 (`restaurar`, os modos, as travas, os valores, o `toque`), e
 * responda com [`quall_camera_host_set_settings`] (com o `"n"`) ou [`quall_camera_host_reject`].
 * `buf` nulo com `cap > 0`: `QUALL_STATUS_NULL_POINTER`.
 *
 * # Safety
 *
 * `h` precisa vir de [`quall_camera_host_new`]; `buf` precisa ser nulo ou ter `cap` bytes.
 */
intptr_t quall_camera_host_next_request(const struct QuallCameraHost *h,
                                        char *buf,
                                        uintptr_t cap);

/**
 * **A bombeada do filmador numa sessão de vídeo.** Manda o que está devido a ela (capacidades,
 * o estado a cada mudança e a cada segundo, as recusas), espera até `timeout_ms` por mensagem,
 * trata o que chegou, e escreve em `changed` (pode ser nulo) os bits de
 * [`QuallCameraHostChange`].
 *
 * Chame em laço, da thread de **cada** sessão, com o `QuallMessages` dela, junto com
 * `quall_session_next_event(s, 0)`. `timeout_ms` de 0 a 250; os relógios do batimento e do lido
 * andam pelo relógio, não pela espera, então `0` serve a quem já tem laço próprio (o OBS).
 * **Um leitor por sessão**: numa sessão de vídeo, ninguém mais chama `quall_messages_next`.
 *
 * `QUALL_STATUS_CLOSED` vem **com** `changed` preenchido: a sessão acabou, a fila foi lida até o
 * fim, e o filmador já a esqueceu.
 *
 * # Safety
 *
 * `h` precisa vir de [`quall_camera_host_new`], `m` de [`quall_session_messages`]; `changed`
 * precisa ser nulo ou apontar para um `uint32_t`.
 */
enum QuallStatus quall_camera_host_pump(const struct QuallCameraHost *h,
                                        const struct QuallMessages *m,
                                        uint32_t timeout_ms,
                                        uint32_t *changed);

/**
 * **Esquece a sessão deste handle**: a casca largou a sessão sem bombear até o
 * `QUALL_STATUS_CLOSED`. Opcional — o filmador também esquece sozinho a sessão que acabou, quando
 * uma nova chega —, mas libera a vaga na hora.
 *
 * # Safety
 *
 * `h` precisa vir de [`quall_camera_host_new`], `m` de [`quall_session_messages`].
 */
enum QuallStatus quall_camera_host_forget(const struct QuallCameraHost *h,
                                          const struct QuallMessages *m);

/**
 * **O estado para a tela do filmador**, como JSON. Padrão `(buf, cap)`: repita até caber.
 *
 * ```json
 * {"permite":false,"camera":1,"cap":1,"versao":17,"controlado_por":{"nome":"OBS no Dell","ha_ms":820},
 *  "receptores":[{"nome":"OBS no Dell","id":"dell-7f2a"}],"pedidos_na_fila":0,"contadores":{…}}
 * ```
 *
 * `"controlado_por"` é nulo fora dos 4 s depois de um pedido aplicado: enquanto não for, a tela
 * mostra "Controlado por <nome>".
 *
 * # Safety
 *
 * `h` precisa vir de [`quall_camera_host_new`]; `buf` precisa ser nulo ou ter `cap` bytes.
 */
intptr_t quall_camera_host_state_json(const struct QuallCameraHost *h,
                                      char *buf,
                                      uintptr_t cap);

/**
 * **Cria o controle da câmera do outro lado**, para uma sessão de recepção de vídeo. Recomeça
 * sozinho na bombeada de uma sessão nova. Libere com [`quall_camera_remote_free`].
 */
struct QuallCameraRemote *quall_camera_remote_new(void);

/**
 * Libera o controle. Nulo é aceito.
 *
 * # Safety
 *
 * `r` precisa ser nulo ou vir de [`quall_camera_remote_new`], e não ser usado depois.
 */
void quall_camera_remote_free(struct QuallCameraRemote *r);

/**
 * **Pede um ajuste parcial**: um objeto JSON com **só** os campos que a pessoa mexeu, nos nomes
 * do registro do R9 (`{"exposicao":"manual","iso":800}`). Conferido na hora contra as
 * capacidades que chegaram: o que o filmador recusaria, ou com `"situacao"` diferente de
 * `"pronto"`, é `QUALL_STATUS_INVALID`. Sai na hora (no máximo 15 por segundo; o que se junta no
 * intervalo sai no envio seguinte). **Só de gesto da pessoa**, nunca ao carregar valores salvos.
 * Qualquer thread.
 *
 * # Safety
 *
 * `r` precisa vir de [`quall_camera_remote_new`]; `settings_json`, uma string C válida.
 */
enum QuallStatus quall_camera_remote_request(const struct QuallCameraRemote *r,
                                             const char *settings_json);

/**
 * **"Restaurar automático"** na câmera do outro lado.
 *
 * # Safety
 *
 * `r` precisa vir de [`quall_camera_remote_new`].
 */
enum QuallStatus quall_camera_remote_restore(const struct QuallCameraRemote *r);

/**
 * **Um toque na imagem** que o receptor mostra, para focar e medir ali (R9 §4.4): `x` e `y` de 0
 * a 1 **no quadro decodificado**, antes de qualquer transformação deste lado (escala, tarja,
 * espelho); um toque fora do quadro não se manda. `long_press` trava, como o toque longo.
 * Sem `"toque"` nas capacidades, ou fora de `[0, 1]`: `QUALL_STATUS_INVALID`.
 *
 * # Safety
 *
 * `r` precisa vir de [`quall_camera_remote_new`].
 */
enum QuallStatus quall_camera_remote_touch(const struct QuallCameraRemote *r,
                                           double x,
                                           double y,
                                           bool long_press);

/**
 * **A bombeada do receptor.** Numa sessão nova esquece a anterior; manda o `ola` e o pedido
 * devidos, espera até `timeout_ms`, trata o que chegou, e escreve em `changed` (pode ser nulo) os
 * bits de [`QuallCameraRemoteChange`]. Mesmas regras de [`quall_camera_host_pump`]: laço na
 * thread da sessão, `timeout_ms` de 0 a 250, um leitor por sessão, e `QUALL_STATUS_CLOSED` com
 * `changed` preenchido.
 *
 * # Safety
 *
 * `r` precisa vir de [`quall_camera_remote_new`], `m` de [`quall_session_messages`]; `changed`
 * precisa ser nulo ou apontar para um `uint32_t`.
 */
enum QuallStatus quall_camera_remote_pump(const struct QuallCameraRemote *r,
                                          const struct QuallMessages *m,
                                          uint32_t timeout_ms,
                                          uint32_t *changed);

/**
 * **O estado para a tela do receptor**, como JSON. Padrão `(buf, cap)`: repita até caber.
 *
 * ```json
 * {"situacao":"pronto","capacidades":{…},"ajuste":{…},"aplicado":{…},"pendente":{"iso":800},
 *  "lido":{…},"autor":"Pixel do Pessoa","versao":17,
 *  "recusa":{"motivo":"superado","campo":"iso","ha_ms":300},"contadores":{…}}
 * ```
 *
 * `"situacao"`: `esperando`, `sem_resposta`, `sem_camera` (os três: não mostre os controles),
 * `nao_permitido` (controles apagados, com os valores, e "O aparelho não permite controle remoto
 * da câmera") ou `pronto`. `"ajuste"` é o aplicado com o pendente por cima: é o que o painel
 * mostra.
 *
 * # Safety
 *
 * `r` precisa vir de [`quall_camera_remote_new`]; `buf` precisa ser nulo ou ter `cap` bytes.
 */
intptr_t quall_camera_remote_state_json(const struct QuallCameraRemote *r,
                                        char *buf,
                                        uintptr_t cap);

extern int sysctl(int *name,
                  unsigned int namelen,
                  void *oldp,
                  uintptr_t *oldlenp,
                  void *newp,
                  uintptr_t newlen);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

#endif  /* QUALL_H */
