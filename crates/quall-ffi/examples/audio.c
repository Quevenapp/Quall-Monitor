/*
 * As portas de áudio da fronteira C, exercitadas de C.
 *
 * Existe pelo mesmo motivo de `sessao.c`: nenhuma casca do Quall é escrita em Rust — Swift,
 * Kotlin e o C++ do plugin de OBS só enxergam `include/quall.h`. Um teste em Rust prova o
 * núcleo; só um programa em C prova que a fronteira **atravessa**.
 *
 * O que ele confere:
 *
 *   1. `quall_audio_preset_json` devolve os números do preset do núcleo — a casca pergunta em
 *      vez de fixar, que é a regra de uma fonte de verdade só;
 *   2. pedir PCMU muda o relógio (48 kHz -> 8 kHz) e o quadro (960 -> 160 amostras), e não só o
 *      nome do codec;
 *   3. `quall_audio_encoder_new/encode/free` codificam Opus de C, já configurados pelo preset;
 *   4. PCMU **não** ganha encoder, e a recusa diz o que fazer no lugar;
 *   5. `quall_track_send_audio` existe e recusa track nula com motivo legível;
 *   6. `QuallTrackDesc` zerado cai em `QUALL_AUDIO_CODEC_DEFAULT`, que é o comportamento de
 *      antes do campo existir.
 *
 * Quem só vai mandar G.711 precisa de (1), (2) e (5), e de mais nada: o µ-law é uma tabela de
 * consulta de 8 bits que cabe em vinte linhas de Swift. Ver `docs/audio.md` §2.
 *
 * A origem é **tom sintético nosso**, e isso não é detalhe: `docs/audio.md` §8 proíbe abrir o
 * microfone ou o áudio de sistema da máquina do usuário.
 *
 * Compilar no macOS:
 *
 *     cargo build -p quall-ffi
 *     cc -o /tmp/audio crates/quall-ffi/examples/audio.c \
 *        -Icrates/quall-ffi/include -Ltarget/debug -lquall -lc++ \
 *        -framework CoreFoundation -framework Security -framework SystemConfiguration
 *     /tmp/audio
 *
 * No Linux troque os frameworks por `-lstdc++ -lpthread -ldl -lm`.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include "quall.h"

/* O tratador de slot. Roda numa thread da libdatachannel; o `payload` aponta para
   dentro do nucleo e vale so durante a chamada. */
static void ao_receber_slot(const QuallAudioSlot *slot, void *user_data) {
    (void)user_data;
    if (!slot) return;
    switch (slot->order) {
    case QUALL_AUDIO_ORDER_FRAME:
        /* opus_decode(dec, slot->payload, slot->len, pcm, 960, 0); */
        break;
    case QUALL_AUDIO_ORDER_FEC:
        /* So vale chamar decode_fec se o socorro de fato carregar LBRR. Sem isso o
           opus_decode cai na ocultacao de perda EM SILENCIO e devolve sucesso — e a
           casca contaria como "curado" um quadro que o decoder inventou. */
        if (slot->fec_has_lbrr == 1) {
            /* opus_decode(dec, slot->payload, slot->len, pcm, 960, 1); */
        } else {
            /* -1 e "nao sei", 0 e "nao": nos dois casos, PLC. */
        }
        break;
    case QUALL_AUDIO_ORDER_SILENCE:
        /* opus_decode(dec, NULL, 0, pcm, 960, 0);  ocultacao de perda */
        break;
    case QUALL_AUDIO_ORDER_IDLE:
        /* So a porta puxada entrega. Nao ha fluxo: zeros, e nao PLC. A porta empurrada,
           que e a deste tratador, nunca o manda — mas um `switch` sem este caso deixa
           de compilar com -Wswitch -Werror, e foi assim que o portao o achou. */
        break;
    }
}

int main(void) {
    char json[1024];
    if (quall_audio_preset_json(QUALL_TRACK_KIND_MICROPHONE, QUALL_AUDIO_CODEC_DEFAULT,
                                json, sizeof json) < 0) {
        printf("FALHOU preset: %s\n", quall_last_error());
        return 1;
    }
    printf("preset microfone : %s\n", json);

    if (quall_audio_preset_json(QUALL_TRACK_KIND_MICROPHONE, QUALL_AUDIO_CODEC_PCMU,
                                json, sizeof json) < 0) return 1;
    printf("preset PCMU      : %s\n", json);

    QuallAudioEncoder *enc =
        quall_audio_encoder_new(QUALL_TRACK_KIND_MICROPHONE, QUALL_AUDIO_CODEC_DEFAULT);
    if (!enc) { printf("FALHOU encoder: %s\n", quall_last_error()); return 1; }

    /* Tom sintético nosso: quatro notas, amplitude 0,5 FS. Nunca o microfone da máquina. */
    static const double notas[4] = {400, 500, 800, 1000};
    int16_t pcm[960];
    uint8_t out[4096];
    long total = 0;
    int quadros = 0;
    for (int q = 0; q < 100; q++) {
        double hz = notas[(q / 25) % 4];
        for (int i = 0; i < 960; i++) {
            double t = (q * 960.0 + i) / 48000.0;
            pcm[i] = (int16_t)(sin(t * hz * 2 * M_PI) * 16383.0);
        }
        intptr_t n = quall_audio_encoder_encode(enc, pcm, 960, out, sizeof out);
        if (n <= 0) { printf("FALHOU encode %d: %s\n", q, quall_last_error()); return 1; }
        total += n; quadros++;
    }
    quall_audio_encoder_free(enc);
    printf("codificou        : %d quadros, %ld bytes, %.1f kbit/s\n",
           quadros, total, total * 8.0 / (quadros * 0.020) / 1000.0);

    /* PCMU nao ganha encoder aqui, e o erro tem de dizer o que fazer. */
    if (quall_audio_encoder_new(QUALL_TRACK_KIND_MICROPHONE, QUALL_AUDIO_CODEC_PCMU)) {
        printf("FALHOU: PCMU nao devia ganhar encoder\n"); return 1;
    }
    printf("recusa de PCMU   : %s\n", quall_last_error());

    /* A porta de envio existe e recusa nulo com motivo. */
    QuallAudioSample s = {out, 4, 0};
    if (quall_track_send_audio(NULL, &s) != QUALL_STATUS_NULL_POINTER) {
        printf("FALHOU: send_audio devia recusar track nula\n"); return 1;
    }
    printf("send_audio(NULL) : %s\n", quall_last_error());

    /* O campo de codec existe no desc, e zero continua sendo o comportamento de antes. */
    QuallTrackDesc d; memset(&d, 0, sizeof d);
    d.kind = QUALL_TRACK_KIND_MICROPHONE; d.label = "Fala";
    printf("desc zerado      : audio_codec=%d (DEFAULT=%d)\n",
           (int)d.audio_codec, (int)QUALL_AUDIO_CODEC_DEFAULT);

    /* ------------------------------------------------------------------ */
    /* A porta de RECEPCAO de audio: o jitter buffer do nucleo, de C.      */
    /*                                                                     */
    /* Antes dela uma casca C mandava audio e nao recebia — metade de um   */
    /* par. O que chega aqui nao e o pacote cru: e uma ordem por slot de   */
    /* 20 ms, sempre, sem buraco, ja ordenada pelo buffer do nucleo.       */
    /* ------------------------------------------------------------------ */
    if (quall_track_on_audio(NULL, ao_receber_slot, NULL) != QUALL_STATUS_NULL_POINTER) {
        printf("FALHOU: on_audio devia recusar track nula\n"); return 1;
    }
    printf("on_audio(NULL)   : %s\n", quall_last_error());

    /* As tres ordens sao ABI, e uma casca compilada hoje conta com elas. */
    if (QUALL_AUDIO_ORDER_FRAME != 0 || QUALL_AUDIO_ORDER_FEC != 1 ||
        QUALL_AUDIO_ORDER_SILENCE != 2) {
        printf("FALHOU: as ordens do buffer mudaram de valor\n"); return 1;
    }
    printf("ordens do buffer : FRAME=%d FEC=%d SILENCE=%d\n",
           (int)QUALL_AUDIO_ORDER_FRAME, (int)QUALL_AUDIO_ORDER_FEC,
           (int)QUALL_AUDIO_ORDER_SILENCE);

    /* O campo que impede a armadilha do decode_fec: -1 e "nao sei", nao "nao". */
    QuallAudioSlot slot; memset(&slot, 0, sizeof slot);
    printf("slot zerado      : order=%d fec_has_lbrr=%d (tri-estado: -1/0/1)\n",
           (int)slot.order, (int)slot.fec_has_lbrr);

    /* ------------------------------------------------------------------ */
    /* A porta PUXADA (18/09/2026): a casca chama no ritmo do dispositivo  */
    /* de saida. docs/contrato-som-puxado.md fixa os nomes.                */
    /* ------------------------------------------------------------------ */
    if (QUALL_AUDIO_ORDER_IDLE != 3) {
        printf("FALHOU: IDLE tinha de ser 3, no fim do enum\n"); return 1;
    }
    if (quall_audio_playout_new(NULL, false) != NULL) {
        printf("FALHOU: playout_new devia recusar track nula\n"); return 1;
    }
    printf("playout_new(NULL): %s\n", quall_last_error());
    if (quall_audio_playout_pull(NULL, 0, NAN, &slot) != QUALL_STATUS_NULL_POINTER) {
        printf("FALHOU: pull devia recusar reproducao nula\n"); return 1;
    }
    if (!isnan(quall_audio_playout_rate(NULL))) {
        printf("FALHOU: a razao de nada e NAN\n"); return 1;
    }
    if (quall_audio_playout_free(NULL) != QUALL_STATUS_OK) {
        printf("FALHOU: free(NULL) e no-op\n"); return 1;
    }
    int64_t deslocamento = 0;
    if (quall_track_capture_offset_us(NULL, &deslocamento) != -1) {
        printf("FALHOU: deslocamento de track nula e -1\n"); return 1;
    }
    printf("porta puxada     : IDLE=%d, recusas de nulo conferidas\n", (int)QUALL_AUDIO_ORDER_IDLE);

    printf("\nTUDO CERTO\n");
    return 0;
}
