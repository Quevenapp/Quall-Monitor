/*
 * Sessão de vídeo ponta a ponta, escrita em C contra `include/quall.h`.
 *
 * Existe porque nenhuma casca do Quall é escrita em Rust: Swift, Kotlin e o C++ do plugin de OBS
 * só enxergam esta fronteira. Um teste em Rust prova o núcleo; só um programa em C prova que a
 * fronteira **atravessa**. Este aqui:
 *
 *   1. sobe um emissor com uma track de tela, numa thread;
 *   2. conecta como receptor, na thread principal;
 *   3. registra `quall_track_on_frame` e `quall_track_on_idr_request`, com um `user_data`
 *      **alocado**, que é o caso que a fronteira precisa aguentar;
 *   4. manda um IDR de 8 KiB (que exige FU-A) e confere que ele volta **byte a byte**;
 *   5. pede IDR pelo receptor e confere que o pedido chega ao emissor;
 *   6. fecha, **confere o status do fecho** e só então libera o `user_data`.
 *
 * É o esqueleto mínimo de um emissor e de um receptor. Copie daqui.
 *
 * Compilar no macOS:
 *
 *     cargo build -p quall-ffi
 *     cc -o /tmp/sessao crates/quall-ffi/examples/sessao.c \
 *        -Icrates/quall-ffi/include -Ltarget/debug -lquall -lc++ \
 *        -framework CoreFoundation -framework Security -framework SystemConfiguration
 *     /tmp/sessao
 *
 * No Linux troque os frameworks por `-lstdc++ -lpthread -ldl -lm`; no Windows, ligue
 * `quall.lib` e as bibliotecas que `crates/quall-core/build.rs` declara.
 */

#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "quall.h"

#define PORTA 7893
#define PIN "424242"
#define TAM_IDR 8192

/* ------------------------------------------------------------------ estado */

static uint8_t quadro_enviado[TAM_IDR + 64];
static size_t quadro_enviado_len = 0;

static uint8_t quadro_recebido[TAM_IDR + 64];
static size_t quadro_recebido_len = 0;
static bool recebeu_idr = false;

static volatile int pedidos_de_idr = 0;
static volatile int falhas = 0;

/* Monta SPS, PPS e um IDR grande o bastante para o pacotizador ter de usar FU-A. */
static void montar_quadro(void) {
    uint8_t *p = quadro_enviado;
    const uint8_t sps[] = {0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f};
    const uint8_t pps[] = {0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80};
    const uint8_t idr[] = {0, 0, 0, 1, 0x65};

    memcpy(p, sps, sizeof sps); p += sizeof sps;
    memcpy(p, pps, sizeof pps); p += sizeof pps;
    memcpy(p, idr, sizeof idr); p += sizeof idr;
    for (int i = 0; i < TAM_IDR; i++) {
        *p++ = (uint8_t)((i % 254) + 1);
    }
    quadro_enviado_len = (size_t)(p - quadro_enviado);
}

/* ------------------------------------------------- tratadores da fronteira */

/* O `user_data` de uma casca de verdade: um bloco alocado que precisa ser liberado na hora
 * certa. É por causa dele que `quall_session_close` devolve status. */
typedef struct {
    int quadros;
    int pedidos;
} Contexto;

/* Roda numa thread da libdatachannel. Copiar é escolha desta sonda; uma casca de verdade
 * entrega direto ao decoder, sem copiar. */
static void ao_receber_quadro(const QuallFrame *frame, void *user_data) {
    Contexto *ctx = (Contexto *)user_data;
    if (ctx != NULL) {
        ctx->quadros++;
    }
    if (frame == NULL || frame->annexb == NULL) {
        return;
    }
    if (quadro_recebido_len == 0 && frame->len <= sizeof quadro_recebido) {
        memcpy(quadro_recebido, frame->annexb, frame->len);
        quadro_recebido_len = frame->len;
        recebeu_idr = frame->idr;
    }
}

/* Também roda numa thread da libdatachannel: levanta a bandeira e volta. */
static void ao_pedir_idr(void *user_data) {
    Contexto *ctx = (Contexto *)user_data;
    if (ctx != NULL) {
        ctx->pedidos++;
    }
    pedidos_de_idr++;
}

/* ------------------------------------------------------------- emissor */

static QuallSession *sessao_emissor = NULL;

static void *thread_emissor(void *arg) {
    (void)arg;
    /* Zerado antes de preencher, e o exemplo importa mais que o código: é daqui que uma casca
     * nova copia o jeito de montar a struct. `{KIND, "rótulo"}` deixava `audio_codec` de fora e
     * dava certo só porque o C zera o resto de um inicializador parcial — a mesma linha virada
     * em `QuallTrackDesc t; t.kind = …;` passa lixo de pilha e ninguém reclama. Foi assim que o
     * JNI do Android quebrou quando `audio_codec` entrou. */
    QuallTrackDesc track;
    memset(&track, 0, sizeof track);
    track.kind = QUALL_TRACK_KIND_SCREEN;
    track.label = "Tela de teste";

    QuallSessionOptions opcoes;
    memset(&opcoes, 0, sizeof opcoes);
    opcoes.me.device_id = "emissor-c";
    opcoes.me.display_name = "Emissor em C";
    opcoes.me.screen_source = true;
    opcoes.me.sink = false;
    opcoes.pin = PIN;
    opcoes.known_peers_json = NULL;
    opcoes.signaling_port = PORTA;
    opcoes.timeout_ms = 30000;
    opcoes.tracks = &track;
    opcoes.track_count = 1;

    sessao_emissor = quall_host(&opcoes);
    if (sessao_emissor == NULL) {
        fprintf(stderr, "quall_host falhou: %s\n", quall_last_error());
        falhas++;
    }
    return NULL;
}

/* ----------------------------------------------------------------- main */

int main(void) {
    montar_quadro();
    printf("quall protocolo v%u, serviço %s\n", quall_protocol_version(), quall_service_type());

    pthread_t emissor;
    if (pthread_create(&emissor, NULL, thread_emissor, NULL) != 0) {
        fprintf(stderr, "pthread_create falhou\n");
        return 1;
    }
    /* Dá tempo de a sinalização subir antes de o receptor bater na porta. */
    usleep(700 * 1000);

    QuallSessionOptions opcoes;
    memset(&opcoes, 0, sizeof opcoes);
    opcoes.me.device_id = "receptor-c";
    opcoes.me.display_name = "Receptor em C";
    opcoes.me.sink = true;
    opcoes.pin = PIN;
    opcoes.timeout_ms = 30000;

    char endpoint[64];
    snprintf(endpoint, sizeof endpoint, "127.0.0.1:%d", PORTA);
    QuallSession *receptor = quall_connect(endpoint, &opcoes);
    if (receptor == NULL) {
        fprintf(stderr, "quall_connect falhou: %s\n", quall_last_error());
        return 1;
    }
    pthread_join(emissor, NULL);
    if (sessao_emissor == NULL) {
        return 1;
    }
    printf("sessão de pé; pareamento %s\n",
           quall_session_pairing_is_new(receptor) ? "novo" : "retomado");

    char peer[512];
    if (quall_session_peer_json(receptor, peer, sizeof peer) > 0) {
        printf("par: %s\n", peer);
    }

    /* --- track de saída, no emissor --- */
    if (quall_session_track_count(sessao_emissor) != 1) {
        fprintf(stderr, "o emissor devia ter 1 track, tem %zu\n",
                quall_session_track_count(sessao_emissor));
        return 1;
    }
    QuallTrack *saida = quall_session_track(sessao_emissor, 0);
    if (saida == NULL) {
        fprintf(stderr, "quall_session_track falhou: %s\n", quall_last_error());
        return 1;
    }
    Contexto *ctx_emissor = calloc(1, sizeof(Contexto));
    Contexto *ctx_receptor = calloc(1, sizeof(Contexto));
    if (ctx_emissor == NULL || ctx_receptor == NULL) {
        fprintf(stderr, "sem memória para o contexto\n");
        return 1;
    }
    if (quall_track_on_idr_request(saida, ao_pedir_idr, ctx_emissor) != QUALL_STATUS_OK) {
        fprintf(stderr, "quall_track_on_idr_request falhou: %s\n", quall_last_error());
        return 1;
    }

    /* --- track de entrada, no receptor --- */
    QuallTrack *entrada = quall_session_next_track(receptor, 10000);
    if (entrada == NULL) {
        fprintf(stderr, "nenhuma track chegou ao receptor\n");
        return 1;
    }
    char rotulo[128];
    quall_track_label(entrada, rotulo, sizeof rotulo);
    printf("track recebida: kind=%d rótulo=\"%s\"\n", (int)quall_track_kind(entrada), rotulo);
    if (quall_track_kind(entrada) != QUALL_TRACK_KIND_SCREEN) {
        fprintf(stderr, "a track chegou com o tipo errado\n");
        return 1;
    }
    if (quall_track_on_frame(entrada, ao_receber_quadro, ctx_receptor) != QUALL_STATUS_OK) {
        fprintf(stderr, "quall_track_on_frame falhou: %s\n", quall_last_error());
        return 1;
    }

    /* --- manda o quadro até ele voltar, e pede IDR até o pedido chegar --- */
    QuallFrame quadro;
    memset(&quadro, 0, sizeof quadro);
    quadro.annexb = quadro_enviado;
    quadro.len = quadro_enviado_len;
    quadro.timestamp_us = 1000000;
    quadro.idr = true;

    for (int i = 0; i < 300; i++) {
        quall_track_send_frame(saida, &quadro);
        quall_track_request_idr(entrada);
        if (quadro_recebido_len > 0 && pedidos_de_idr > 0) {
            break;
        }
        usleep(20 * 1000);
    }

    printf("\n--- resultado ---\n");
    char stats[256];
    if (quall_track_stats_json(saida, stats, sizeof stats) > 0) {
        printf("emissor : %s\n", stats);
    }
    if (quall_track_stats_json(entrada, stats, sizeof stats) > 0) {
        printf("receptor: %s\n", stats);
    }

    int erro = 0;
    if (quadro_recebido_len == 0) {
        fprintf(stderr, "FALHA: nenhum quadro atravessou\n");
        erro = 1;
    } else if (quadro_recebido_len != quadro_enviado_len ||
               memcmp(quadro_recebido, quadro_enviado, quadro_enviado_len) != 0) {
        fprintf(stderr, "FALHA: o quadro voltou diferente (%zu bytes contra %zu)\n",
                quadro_recebido_len, quadro_enviado_len);
        erro = 1;
    } else {
        printf("quadro de %zu bytes atravessou idêntico (FU-A remontado)\n", quadro_recebido_len);
    }
    if (!recebeu_idr) {
        fprintf(stderr, "FALHA: o quadro não veio marcado como IDR\n");
        erro = 1;
    }
    if (pedidos_de_idr == 0) {
        fprintf(stderr, "FALHA: o pedido de IDR do receptor não chegou ao emissor\n");
        erro = 1;
    } else {
        printf("pedido de IDR chegou ao emissor %d vez(es)\n", pedidos_de_idr);
    }

    /* Ordem de liberação: tracks, sessões, e só então o `cleanup` global — com uma sessão viva
     * ele deixa a thread de limpeza presa e o processo não morre.
     *
     * **O status de `quall_session_close` é o que autoriza liberar o `user_data`.** Com
     * `QUALL_STATUS_OK` não há callback rodando e não haverá mais nenhum; com qualquer outro
     * status o bloco tem de continuar vivo, porque alguém pode estar dentro dele. Este é o
     * pedaço que uma casca de verdade copia daqui. */
    quall_track_free(saida);
    quall_track_free(entrada);

    QuallStatus fecho_receptor = quall_session_close(receptor);
    QuallStatus fecho_emissor = quall_session_close(sessao_emissor);

    if (fecho_receptor == QUALL_STATUS_OK) {
        printf("receptor fechado com barreira: %d quadro(s) pelo user_data\n",
               ctx_receptor->quadros);
        free(ctx_receptor);
    } else {
        fprintf(stderr, "AVISO: o receptor fechou sem barreira (%s); vazando o user_data de "
                        "propósito, que é mais barato que uso-após-liberação\n",
                quall_last_error());
        erro = 1;
    }
    if (fecho_emissor == QUALL_STATUS_OK) {
        printf("emissor fechado com barreira: %d pedido(s) pelo user_data\n",
               ctx_emissor->pedidos);
        free(ctx_emissor);
    } else {
        fprintf(stderr, "AVISO: o emissor fechou sem barreira (%s)\n", quall_last_error());
        erro = 1;
    }

    quall_cleanup();

    printf(erro ? "\nFALHOU\n" : "\nok\n");
    return erro;
}
