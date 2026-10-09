# datachannel-sys 0.23.0+0.23.2, com três mudanças do Quall

Cópia do crate publicado (`datachannel-sys-0.23.0+0.23.2` do crates.io, libdatachannel 0.23.2),
ligada ao workspace por `[patch.crates-io]` no `Cargo.toml` da raiz. O diff inteiro está em
`quall.patch`, e **só quatro arquivos diferem** do publicado (fora este e o `quall.patch`) — confira
com `diff -rq ~/.cargo/registry/src/*/datachannel-sys-0.23.0+0.23.2 vendor/datachannel-sys`.

## Por que existe

Espaçar a saída de vídeo do emissor. Em 11/09/2026 (`docs/tela-estendida.md`, "A sujeira que ficava
até o IDR programado"), com o Mac no cabo e dois receptores no mesmo rádio de 5 GHz, um limitador de
60 Mbit/s na saída do Mac (`dummynet`) levou a perda do iPhone X de 7,4 % para 0,17 %, sem mexer na
taxa média. O espaçador certo para isso é o `rtc::PacingHandler` da própria libdatachannel, que roda
depois da pacotização. Mas em 0.23.2 ele só existe na API C++ — e com um defeito.

## As quatro mudanças

1. **`src/pacinghandler.cpp` — a guarda de `schedule` estava invertida.** `exchange(true)` devolve o
   valor **anterior**. Com `if (!mHaveScheduled.exchange(true)) return;`, a primeira chamada nunca
   agendava: o primeiro quadro esperava o seguinte para sair, e um dreno que precisasse de mais de
   um intervalo parava até chegar outro quadro. Registrado em `docs/bancada.md` desde antes, sem
   conserto porque ninguém usava. Agora `if (mHaveScheduled.exchange(true)) return;`.
2. **`src/pacinghandler.cpp` — `mLastRun` nasce na construção**, e não na época do relógio. O
   efeito antigo era só a primeira verba sair do teto; o novo é ela sair do tempo real.
3. **`src/capi.cpp` e `include/rtc/rtc.h` — `rtcChainPacingHandler(tr, bitsPerSecond,
   sendIntervalMs)`**, no molde dos outros `rtcChain…`. Recusa taxa ou intervalo não positivos com
   `RTC_ERR_INVALID`. O `bindgen` do `build.rs` gera a ligação Rust sozinho a partir do `rtc.h`.
4. **`build.rs` — recompilar quando o C++ muda.** O publicado só emite `rerun-if-env-changed`, e
   isso desliga a regra padrão do cargo ("roda de novo quando qualquer arquivo do pacote muda"):
   **uma edição no C++ desta pasta não era recompilada**, e o teste rodava contra a biblioteca
   velha. Foi assim que o primeiro controle do conserto 1 passou quando devia falhar. Agora ele
   observa `libdatachannel/src` e `libdatachannel/include`.

**O controle do conserto 1**: com a guarda invertida de volta e a biblioteca recompilada,
`transport::tests::espacador_espalha_o_quadro_grande_e_entrega_inteiro` falha com "com espaçador o
quadro não chegou" em 20 s. Com o conserto, passa em ~2 s.

O espaçador tem de ser **o último** da corrente: ele segura os pacotes e os entrega direto ao
transporte, e o que viesse depois dele não os veria.

## Para largar este fork

Quando sair um `datachannel-sys` com libdatachannel que traga `rtcChainPacingHandler` e a guarda
consertada: apagar esta pasta e o `[patch.crates-io]`, e conferir se a assinatura da função é a
mesma que `crates/quall-rtc` chama.
