//! Os argumentos de linha de comando do app.
//!
//! # Por que um app de produto tem modo de bancada
//!
//! Porque a única forma de rodar qualquer coisa visual neste Dell é por Tarefa Agendada na sessão
//! interativa (`docs/windows-acesso.md`) — o SSH cai na Sessão 0, onde o
//! `Windows.Graphics.Capture` devolve `0x80070424` e uma janela não tem para onde desenhar. Numa
//! tarefa agendada **não há mão humana**: ninguém clica em Espelhar, ninguém lê o PIN da tela para
//! digitar do outro lado, ninguém fecha a janela.
//!
//! `--espelhar-ja`, `--pin` e `--sair-apos` existem para isso e só para isso. No caminho de
//! produto o PIN é sempre sorteado pelo núcleo — é a qualidade do sorteio que segura o pareamento.

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Clone)]
#[command(
    name = "quall-app",
    about = "Quall — espelhar a tela deste computador para outro aparelho da rede."
)]
pub struct Argumentos {
    /// Arquivo de registro. Sem a flag, `%LOCALAPPDATA%\Quall\Logs\quall-app.log`.
    #[arg(long)]
    pub registro: Option<PathBuf>,

    /// **Bancada.** PIN fixo, para uma corrida automatizada não precisar ler a tela.
    #[arg(long)]
    pub pin: Option<String>,

    /// Fonte a transmitir: o nome do dispositivo (`\\.\DISPLAY1`), o nome do monitor, o link ou o
    /// nome de uma câmera, ou `tela` para o primeiro **monitor**. **Casamento exato; sem casamento o
    /// app sai com erro** e nunca escolhe outra no lugar (`catalogo_de_cameras::escolher_inicial`).
    #[arg(long)]
    pub fonte: Option<String>,

    /// **Aceita e ignorada desde a fase 5** de `docs/camera-no-windows.md`: as câmeras deste PC
    /// entram no seletor **por padrão**, depois dos monitores, sem as do próprio Quall (a decisão 6
    /// do Bruno, 21/09). Fica escondida para os roteiros de bancada das fases 1–4 (`camera-r4.ps1`,
    /// `camera-r5.ps1`, `fase4.ps1`) continuarem rodando; ninguém a lê.
    #[arg(long = "cameras", hide = true)]
    pub cameras_ignorada: bool,

    /// **Bancada.** O seletor **sem câmeras**, como antes da fase 5: nem a enumeração do Media
    /// Foundation, nem o registro do dono de cada câmera, nem o `WM_DEVICECHANGE` delas. Para as
    /// corridas de tela que não querem o catálogo de câmeras no caminho, e para isolar um defeito
    /// que se suspeite dele.
    #[arg(long, conflicts_with = "camera_de_bancada")]
    pub sem_cameras: bool,

    /// **Bancada, fase 4** (`docs/camera-no-windows.md` §7.3). Libera no seletor **só este link**, e
    /// só se ele for a câmera de bancada da sonda: dono Quall, nome "<base> sonda <PID>", e o PID de
    /// um `quall_camera_local.exe` vivo. Qualquer outra câmera do Quall (as baias do `Quall.exe` do
    /// usuário, que mostram o celular dele) continua fora. Com `--fonte` igual a ele, a abertura
    /// espera o link aparecer na enumeração, até 10 s, antes de escolher.
    #[arg(long)]
    pub camera_de_bancada: Option<String>,

    /// **Bancada.** Transmite a **fonte do Quall instanciada neste processo** como câmera (sem nó
    /// de dispositivo e sem Frame Server), com um nome aleatório cujo cano **este processo serve**,
    /// depois de conferir que ninguém o serve, com o quadro de bancada com a régua
    /// (`regua_de_bancada`): conteúdo nosso, e o receptor com `--regua` confere o pixel. Com
    /// `--varias-sessoes`, toda sessão; numa sessão só, no lugar da escolha. Não abre câmera de
    /// ninguém.
    #[arg(long)]
    pub camera_sintetica: bool,

    /// **Bancada, fase 5.** Não desentrelaça a câmera entrelaçada (o DV da Panasonic): o quadro
    /// segue como veio, com os dois campos tecidos. Existe para o A/B de olho (o Bruno compara a
    /// janela com e sem) e do custo; o produto sempre desentrelaça.
    #[arg(long)]
    pub sem_desentrelacar: bool,

    /// **Bancada, 22/09.** Quem desentrelaça o DV: `adapt2` (o padrão: adaptativo ao movimento, na
    /// CPU, com o leitor do DV sem o gerenciador D3D) ou `bob` (o caminho da fase 5: o leitor com o
    /// gerenciador e o bob do processador de vídeo, meia resolução vertical). Existe para o A/B do
    /// Bruno com a Panasonic e o OBS. O `--sem-desentrelacar` manda sobre os dois.
    #[arg(long, default_value = "adapt2", value_parser = ["adapt2", "bob"])]
    pub desentrelacador: String,

    /// **Bancada, a câmera que pausa (22/09).** Janelas `início:duração` em segundos, separadas por
    /// vírgula (`20:15,60:40`), contadas da criação da captura (de cada tentativa de abertura), em
    /// que a captura da câmera descarta o que o leitor entrega, como se a câmera tivesse parado.
    /// Com a `--camera-sintetica`, prova a pausa sem câmera de ninguém. Simula **na nossa fronteira**: o driver de uma câmera de verdade
    /// (o leitor que volta, ou não) não é exercitado. O produto não usa.
    #[arg(long, hide = true, value_parser = pausas_de_bancada)]
    pub pausar_camera: Option<PausasDeBancada>,

    /// **Bancada.** Imprime o catálogo de fontes (monitores e câmeras, com o dono de cada câmera e
    /// se ela entra) e sai — **antes** de subir as câmeras virtuais das baias, da janela e da rede.
    /// Não ativa câmera nenhuma.
    #[arg(long)]
    pub listar_fontes: bool,

    /// **Bancada.** Os retratos da janela principal (`docs/telas-estudio.md` §9): cada estado de
    /// exemplo desenhado pela janela de verdade, fora da tela, num bitmap nosso, gravado em BMP nesta
    /// pasta — e sai. Sem emissor, receptor, câmera virtual nem rede; roda na Sessão 0 pelo SSH, sem
    /// tocar na área de trabalho de ninguém.
    #[arg(long)]
    pub retratos_de_bancada: Option<PathBuf>,

    /// **Bancada.** Clica em Espelhar sozinho assim que a janela abre.
    #[arg(long)]
    pub espelhar_ja: bool,

    /// **Bancada.** Pedir IDR também reinicia o fluxo do MFT (`FLUSH` + `NOTIFY_START_OF_STREAM`).
    ///
    /// É a terceira porta para o defeito do Quick Sync, que aceita `AVEncVideoForceKeyFrame` com
    /// `S_OK` e ignora. Fica atrás de flag porque o MFT é assíncrono e o `FLUSH` invalida os
    /// créditos de `METransformNeedInput`: se o MFT não voltar a pedir entrada, o emissor **para**.
    /// Um caminho que pode travar o produto não entra ligado por padrão antes de a bancada medir
    /// que ele não trava.
    #[arg(long)]
    pub idr_por_flush: bool,

    /// **Desliga** a quinta porta, que desde 2026-08-28 é o padrão do produto.
    ///
    /// A quinta porta é: pedir IDR **derruba o transform do encoder e monta outro**. As quatro
    /// portas anteriores pediam ao MFT que mudasse de comportamento e ele não mudou. Esta não
    /// pede: um encoder recém-criado não tem cadeia de referência anterior, então o primeiro
    /// quadro dele é obrigatoriamente IDR. A captura, o dispositivo D3D11 e o escalador **não**
    /// são derrubados junto — só o encoder.
    ///
    /// # Por que ligada por padrão, e o que a bancada mediu antes de ligar
    ///
    /// Ela nasceu atrás de flag, desligada, pelo mesmo motivo da terceira porta: o MFT é assíncrono
    /// e a `Cadeia` guarda créditos de `METransformNeedInput`. "Mais simples" não é "medido". Foi
    /// medida:
    ///
    /// - 20 recriações numa sonda e **54 numa sessão real de 180 s**, com `desligamentos_sujos=0`,
    ///   `recusados=0` e montagem estável de 146 a 160 ms da primeira à última;
    /// - conjunto de parâmetros **byte a byte idêntico** (53 iguais, 0 diferentes) — um SPS novo
    ///   com valores diferentes no meio da sessão quebraria receptor, e não acontece;
    /// - ponta a ponta, a tela preta depois de uma perda caiu de **10.162 ms para 251,5 ms**, com
    ///   `resolvidas_sem_pedido=0`: as 46 quebras foram resolvidas por pedido atendido, nenhuma
    ///   por espera.
    ///
    /// # O que continua sem resolver, e por isso está escrito aqui
    ///
    /// Cada recriação vaza **+3,057 handles** no processo (eram +13 antes de `ShutdownObject`; a
    /// origem dos 3 restantes **continua sem ser achada**). Medido em 2026-08-29 sobre 614
    /// recriações em quinze sessões — os braços sem recriação nenhuma crescem 2 a 10 handles em
    /// 127 s, que é a deriva própria da sessão. **O vazamento é por recriação, não por tempo.**
    ///
    /// # E o preço em rede, que só foi medido em 2026-08-29
    ///
    /// Esta porta **é a causa** da perda do emissor do Windows. Três braços, n=3 cada,
    /// intercalados, receptor com o contador exato:
    ///
    /// | braço | bitrate | pacotes/s | perdidos / entregues | perda |
    /// |---|---|---|---|---|
    /// | porta LIGADA, alvo 4 Mbps | 1,11 Mbps | 220 | 1.991 / 83.936 | **2,373 %** |
    /// | porta desligada, alvo 1 Mbps | 0,92 Mbps | 220 | 160 / 84.186 | 0,190 % |
    /// | porta desligada, alvo 1,3 Mbps | **1,20 Mbps** | **251** | 74 / 96.040 | **0,077 %** |
    ///
    /// **12,5× com a taxa de pacotes igualada, e 31× contra um braço que manda mais bits e mais
    /// pacotes.** "Perde menos porque manda menos" está descartado nas duas direções. Ela fabrica
    /// cerca de dois terços das quebras de imagem que existe para consertar — 73 eventos de perda
    /// para 79 recriações por sessão.
    ///
    /// **E mesmo assim ela continua ligada por padrão**, porque pelo que a pessoa vê ela ganha:
    /// a imagem passa ~15 % do tempo quebrada com ela, contra ~52 % sem ela a bits iguais. Ela
    /// conserta em 250 ms um estrago que sem ela leva 3 s para se resolver sozinho. O conserto de
    /// verdade não é desligá-la nem limitá-la (ver `piso_entre_recriacoes_ms`): é uma recriação
    /// que **não pare o fluxo**. Ver `docs/quinta-porta.md`.
    ///
    /// A flag existe no negativo para a bancada poder medir o braço sem a porta — um A/B com uma
    /// variável só —, e para haver como desligar em campo se o vazamento morder.
    #[arg(long)]
    pub sem_idr_por_recriacao: bool,

    /// Piso de tempo entre duas recriações de encoder, em milissegundos. `0` (o padrão) é sem
    /// piso — a quinta porta recria a cada pedido que ela não dispensa por outro motivo.
    ///
    /// # O que este número troca por quê
    ///
    /// A quinta porta é ligada e a sessão passa a recriar o encoder **69 a 75 vezes** em 110 s,
    /// cada recriação com ~150 ms de laço parado: ~10 s de uma janela de 110 s sem consumir
    /// captura. O piso é um limitador de taxa em cima disso — não uma recusa. Um pedido que
    /// chega dentro do piso **fica guardado** e é atendido assim que o piso vence
    /// (`pedido_adiado`), então o que o piso muda é *quando* o quadro-chave sai, nunca *se* ele
    /// sai.
    ///
    /// Isso troca recuperação por perda de forma graduada: quanto maior o piso, menos laço
    /// parado e menos tráfego de IDR, e mais tempo de imagem quebrada entre a perda e o
    /// quadro-chave que a conserta. O piso nunca pode ficar acima do que o produto aceita de
    /// tela parada.
    ///
    /// # O padrão é 0, e o 0 foi ESCOLHIDO por medição — não é cautela
    ///
    /// A curva foi medida em 2026-08-29 (`docs/quinta-porta.md`, achado 2), três pontos, n=3 cada,
    /// intercalados, receptor `quall-probe` no A10s com a política de perda do produto:
    ///
    /// | piso | perda exata | bitrate | recriações | sem_ref p50 | tempo com imagem quebrada |
    /// |---|---|---|---|---|---|
    /// | **0 ms** | **2,42 %** | 1,05 Mbps | 82 | **254 ms** | **~15 %** |
    /// | 1.500 ms | 1,99 % | 1,06 Mbps | 74 | 305 ms | ~15,5 % |
    /// | 4.000 ms | 4,14 % | 2,56 Mbps | 32 | 2.640 ms | ~55 % |
    ///
    /// **1.500 ms é um empate** — a perda cai um pouco, o tempo quebrado sobe um pouco, e o saldo
    /// fica no ruído. **4.000 ms é pior nos dois eixos**, e o motivo não estava na previsão: a
    /// taxa de recriação **governa o bitrate entregue**, porque menos recriação é mais tempo para
    /// o controle de taxa do MFT subir a rampa. Com um terço das recriações o emissor passa a
    /// mandar 2,56 Mbps em vez de 1,05 — e mandar mais perde mais. O piso não desliga a porta: ele
    /// a desliza para um meio-termo que herda o pior dos dois lados.
    ///
    /// **O que se sacrifica ligando este piso é recuperação, e não se ganha perda em troca.** Por
    /// isso o padrão é 0. Um piso só teria sentido acompanhado de um alvo de bitrate preso
    /// (`--bitrate-alvo`), que é outra configuração e outro experimento — não medido.
    ///
    /// A flag existe no positivo (e não como `--sem-piso`) porque o padrão é não ter piso.
    #[arg(long, default_value_t = 0)]
    pub piso_entre_recriacoes_ms: u64,

    /// **Bancada.** Alvo de bitrate do encoder, em bits por segundo. O padrão de produto é
    /// `4_000_000` (preset de tela, `docs/ux-m6.md` tarefa 3).
    ///
    /// # Por que um A/B da quinta porta precisa disto
    ///
    /// Os dois braços do A/B **não põem o mesmo tráfego no fio**, e isso invalidou a comparação
    /// de 28/08: com a porta ligada o emissor manda ~1,2 Mbps; com ela desligada, ~3,7 Mbps —
    /// contra o mesmo alvo de 4 Mbps configurado nos dois. A causa é a própria porta: cada
    /// recriação zera o controle de taxa do MFT, que recomeça a rampa e nunca chega ao alvo.
    ///
    /// O efeito é que "o braço com a porta perde menos" fica indistinguível de "o braço com a
    /// porta **manda** menos". `--taxa-de-entrega` não resolve: ela iguala quadros por segundo,
    /// não bits. Este sinalizador resolve, pelo outro lado — baixando o alvo do braço **sem** a
    /// porta até o bitrate que o braço **com** a porta de fato entrega, os dois passam a carregar
    /// a mesma coisa e a única variável que sobra é a recriação.
    ///
    /// Não encosta em `PERFIL_H264` nem no `fmtp`: é o `MF_MT_AVG_BITRATE` do tipo de saída, que
    /// o receptor não lê.
    ///
    /// # O que ele mediu, em 2026-08-29
    ///
    /// Com o alvo baixado para 1 Mbps e a porta desligada, o emissor entregou **a mesma taxa de
    /// pacotes** que o braço de produto (220 contra 220 pacotes/s; 84.186 contra 83.936 pacotes em
    /// três corridas — 0,3 % de diferença) e perdeu **160 pacotes contra 1.991**. Doze vezes e
    /// meia, sem normalizar nada.
    ///
    /// É o número que fecha a pergunta "a porta perde menos ou manda menos?": ela **manda** menos
    /// *e* **perde** doze vezes mais. Ver `docs/quinta-porta.md`, achado 1.
    /// # O padrão deixou de ser um número em 02/09/2026
    ///
    /// Era `4_000_000` cravado aqui, e esse literal existia igual em mais quatro cascas. Quando o
    /// teto de resolução subiu para 1080p em 01/09, nenhum deles subiu junto: 2,25 vezes os
    /// pixels pelo mesmo orçamento de bits. Agora, **sem o sinalizador**, o alvo vem de
    /// `quall_core::teto::ajustar` — que já é consultado logo acima, para a geometria — e
    /// acompanha o quadro que de fato vai sair. Com o sinalizador, manda quem chamou, que é o que
    /// a bancada precisa.
    #[arg(long)]
    pub bitrate_alvo: Option<u32>,

    /// **Bancada.** De quanto em quanto tempo o laço pergunta à sessão se o outro lado ainda está
    /// lá. `0` — o padrão — é **a cada volta**, que é o comportamento correto.
    ///
    /// # Isto existiu como desvio, e o desvio acabou
    ///
    /// Até 30/08 este valor era 200 ms, fixo no código, e o comentário dele dizia em letras que
    /// era **desvio e não conserto**: `proximo_evento(Duration::ZERO)` custava **29,43 ms por
    /// volta** no Windows — três quartos do laço, numa chamada cujo argumento é zero — porque o
    /// núcleo elevava fatia zero a 1 ms e o `SO_RCVTIMEO` do Windows acorda no tique de ~15,6 ms,
    /// duas vezes (socket e canal do transporte).
    ///
    /// **O defeito foi consertado no núcleo em 29/08** (`docs/laco-e-qualidade.md`:
    /// `signaling.rs::poll_por` põe o socket em não bloqueante e `transport.rs` usa `try_recv`),
    /// e o conserto foi **conferido no Dell** em 30/08, que é onde o sintoma existia:
    /// `proximo_evento` passou de 29,43 ms para **0,03 ms** por chamada — mil vezes. A 80 voltas
    /// por segundo, espiar a cada volta custa 2,4 ms por segundo de parede.
    ///
    /// Com o desvio o app percebia o outro lado sair com até 200 ms de atraso; sem ele, na volta
    /// seguinte. A alternativa a este detector continua sendo os 30 s do `CONSENT_TIMEOUT` do
    /// libjuice, então os 200 ms nunca foram graves — mas eram um teto artificial no laço, e não
    /// há mais motivo para pagá-lo.
    ///
    /// A flag fica, com o padrão em 0, para a bancada poder **remedir o desvio** sem recompilar:
    /// `--espiada-ms 200` reproduz o comportamento de antes, e é assim que o A/B de 30/08 rodou
    /// os dois braços com um binário só.
    #[arg(long, default_value_t = 0)]
    pub espiada_ms: u64,

    /// A quinta porta troca o encoder **por ponteiro**, em vez de derrubar e montar dentro do
    /// laço. Ligada por padrão desde 30/08; `--sem-troca-a-quente` volta ao caminho antigo.
    ///
    /// # O que ela troca, e o número que sustenta
    ///
    /// A quinta porta conserta a imagem quebrada em ~250 ms e, para isso, **para o laço por ~150
    /// ms** a cada recriação — 37 a 40 vezes por minuto. Esse silêncio era uma das três
    /// candidatas ao mecanismo da perda de 2,4 % (as outras duas: a rajada do IDR e o controle de
    /// taxa reiniciado).
    ///
    /// A decomposição dos 150 ms, medida no Quick Sync do Dell por `quall-gemeos` (n=10, mediana):
    /// `desligar` 18,0 ms + `ActivateObject` 74,2 ms + `configure` 45,1 ms + abrir o fluxo 1,1 ms.
    /// **Nenhum desses passos precisa do quadro que está passando.** Montados numa thread de
    /// fundo, o que sobra no laço é trocar dois ponteiros: **0 µs**.
    ///
    /// Medido antes de existir código de produto, e por isso a ideia sobreviveu:
    ///
    /// - **dois MFTs de hardware coexistem** no mesmo `IMFDXGIDeviceManager` e no mesmo
    ///   dispositivo D3D11, os dois codificando ao mesmo tempo, nenhum `ProcessInput` recusado;
    /// - o encoder de reserva ocioso custa **+16 handles** e ~12 MB;
    /// - vinte trocas a quente deixaram **+3,05 handles por troca** — o **mesmo** número da
    ///   recriação de hoje. A reserva não dobra o vazamento;
    /// - `IMFActivate::ActivateObject` devolve o objeto **em cache** enquanto ele estiver vivo, e
    ///   é por isso que a oficina alterna entre **duas** vagas.
    ///
    /// # O que se sacrifica
    ///
    /// Uma thread a mais, um encoder a mais vivo (+16 handles, ~12 MB), e um caminho de exceção:
    /// se a reserva não estiver pronta na hora do pedido, a porta **cai para o caminho antigo**
    /// em vez de deixar o receptor sem quadro-chave. `trocas_sem_reserva` conta essas vezes.
    ///
    /// Ver `oficina.rs` e `docs/troca-a-quente.md`.
    #[arg(long)]
    pub sem_troca_a_quente: bool,

    /// **Bancada.** Encerra o processo N segundos depois de a transmissão começar.
    #[arg(long)]
    pub sair_apos: Option<u64>,

    /// Porta de sinalização. `0` (o padrão) deixa o sistema escolher uma livre e o app publica a
    /// que saiu — é o que fecha a corrida entre reservar a porta e hospedar nela.
    #[arg(long, default_value_t = 0)]
    pub porta: u16,

    /// Taxa de quadros alvo **do encoder** — e só dele.
    ///
    /// # Este sinalizador não governa a taxa entregue, e isso foi medido
    ///
    /// O nome diz "taxa de quadros alvo" e por dois dias a bancada leu isso como "quantos quadros
    /// por segundo saem no fio". Não é o que ele faz. Lendo o caminho inteiro, este número entra
    /// em exatamente três lugares, todos dentro de `transmissao.rs::Cadeia::abrir`:
    ///
    /// - `teto::ajustar(largura, altura, fps)`, que o usa para a cobrança de `MaxMBPS` do nível;
    /// - `EncoderConfig::fps`, que é o alvo do controle de taxa do MFT;
    /// - `gop_frames` e `duracao_100ns`, que são declarações, não portas.
    ///
    /// **Nenhum deles é uma porta no caminho do quadro.** `bombear` entrega ao encoder tudo o que
    /// o `Windows.Graphics.Capture` puser na caixa postal, e quem limita a taxa hoje é o laço:
    /// com o padrão sintético animado o WGC oferece 45,7 fps e saem ~23. Pedir `--fps 12` não
    /// entrega 12 — entrega os mesmos ~23, com o encoder configurado para 12.
    ///
    /// A porta de verdade é [`Argumentos::taxa_de_entrega`], e ela é deliberadamente **separada**
    /// deste número: variar `--fps` mexeria ao mesmo tempo na taxa entregue, no orçamento de bits
    /// por quadro e no GOP pedido, e uma curva medida assim não teria uma variável só.
    #[arg(long, default_value_t = 30)]
    pub fps: u32,

    /// **Bancada.** Limita quantos quadros por segundo o laço entrega ao encoder. Sem ele, sem
    /// porta nenhuma — que é o comportamento de produto de hoje.
    ///
    /// Existe para medir a curva de fps × perda com **uma** variável. Ele não encosta em
    /// `EncoderConfig`: o encoder continua configurado com `--fps`, o bitrate continua 4 Mbps e o
    /// GOP pedido continua o mesmo. O que muda é só quantos quadros capturados atravessam.
    ///
    /// # Por que a porta é por prazo acumulado, e não por intervalo mínimo
    ///
    /// A primeira versão recusava o quadro que chegasse antes de `1/taxa` do anterior. Medido no
    /// papel antes de escrever: com o WGC entregando a 45,7 fps (21,9 ms), um intervalo mínimo só
    /// consegue produzir 45,7/n — 22,8, 15,2, 11,4 fps e nada entre eles. Pedir 20 e pedir 16
    /// dariam **o mesmo** 15,2, e a curva teria três pontos onde eu queria cinco.
    ///
    /// Esta versão guarda o instante do próximo quadro devido e o avança de `1/taxa` a cada
    /// entrega, em vez de reancorá-lo no quadro que passou. A taxa média fica na que se pediu; o
    /// que sobra de erro é o jitter de um intervalo de captura. Quando a fonte para e volta (o
    /// WGC entrega o que a tela muda, e uma tela parada não muda), o alvo é reancorado para o
    /// laço não cuspir uma rajada de recuperação — que seria exatamente a doença em estudo.
    #[arg(long)]
    pub taxa_de_entrega: Option<u32>,

    /// **Bancada.** Uma caixa postal de uma posição no caminho do quadro, em vez de duas em série.
    ///
    /// # O que ele muda
    ///
    /// Hoje há **dois** mailboxes de uma posição entre o `Windows.Graphics.Capture` e o
    /// `ProcessInput`: o slot do WGC (em `capture.rs`) e o `pendente` do laço (em
    /// `transmissao.rs`). O laço tira o quadro do primeiro assim que é avisado, e o guarda no
    /// segundo até o MFT pedir entrada. Como o MFT pede ~30 vezes por segundo e o WGC entrega
    /// 45 a 53, o quadro guardado costuma ser sobrescrito antes de ser usado — 1 050 vezes numa
    /// sessão de 76 s, medido em 31/08 (`docs/quadros-que-nao-saem.md`).
    ///
    /// Com esta flag o laço **não tira** o quadro no aviso: ele deixa no slot do WGC e só o tira
    /// na hora de submeter. O descarte continua acontecendo, no mesmo tanto — o que muda é que o
    /// quadro que atravessa é o mais novo que existe **no instante da submissão**, e não o mais
    /// novo que existia no último aviso.
    ///
    /// # É um NEGATIVO MEDIDO: não ligue esperando ganho
    ///
    /// A previsão era latência — o quadro guardado em `pendente` envelheceria meio intervalo de
    /// captura antes de ser submetido, ~11 ms a 45 quadros/s. **O A/B de 31/08 derrubou a
    /// previsão**: quatro corridas intercaladas deram `latencia_media_ms` de **26,52 ms com a
    /// flag contra 27,00 ms sem ela**, e o braço de produto sozinho espalha 1,04 ms entre as suas
    /// duas corridas. Está no ruído.
    ///
    /// O motivo, depois de medido: o laço consome o aviso de `frame_ready` prontamente, então
    /// `pendente` e o slot do WGC contêm quase sempre o **mesmo** quadro. Os 26,5 ms são o
    /// encoder (~13 ms de `espera_da_saida`) mais a espera pelo crédito (~13 ms), e nenhum dos
    /// dois é a caixa postal.
    ///
    /// Vazão também não muda, e não podia mudar: quem a governa é o MFT, que oferece **um**
    /// crédito de cada vez e demora ~30 ms para oferecer o seguinte.
    ///
    /// O que ela faz de verdade é contábil: o descarte sai do `pendente`, que ninguém contava
    /// antes desta frente, e vai para o slot do WGC, que já era contado — e a razão
    /// `encodados/capturados` passa a ler 100 %. Isso não vale mexer no caminho do quadro do
    /// produto, e por isso ela fica desligada. Ver `docs/quadros-que-nao-saem.md` §6.
    #[arg(long)]
    pub caixa_unica: bool,

    /// **Bancada.** Liga o registro interno da libdatachannel/libjuice no registro do app.
    ///
    /// É a testemunha do **caminho de saída**, e hoje ela é a única que existe. O socket de saída
    /// é não-bloqueante; quando o buffer de envio enche, a libjuice registra
    /// `Send failed, buffer is full` e devolve erro — e `Track::outgoing` da libdatachannel
    /// **sobrescreve** o retorno a cada fragmento (`impl/track.cpp:187-199`, a dívida 30), então
    /// `enviar_quadro` devolve `Ok(())` com o número de sequência já gasto. Sem esta flag o
    /// descarte não aparece em lugar nenhum: `log::max_level()` é `Off` por omissão e o
    /// `rtcInitLogger` do crate `datachannel` mapeia isso para `RTC_LOG_NONE`.
    ///
    /// Desligada por padrão porque **liga escrita no caminho quente** — não por pacote (nem
    /// `Info` da libdatachannel nem o da libjuice registram por pacote), mas o princípio da casa
    /// é não mudar o padrão do produto sem medir os dois lados.
    #[arg(long)]
    pub registro_da_biblioteca: bool,

    /// Começa com "Transmitir o som deste computador" **desmarcado**.
    ///
    /// O padrão é **com som**, e a escolha tem argumento: quem espelha a tela inteira já está
    /// mandando tudo o que ela mostra, e um vídeo que chega mudo é o resultado surpreendente, não o
    /// contrário. O que a privacidade exige não é o padrão desligado — é que a opção esteja
    /// **visível na mesma tela do botão**, antes do clique, e nunca escondida atrás de um menu. É
    /// o que a caixa de seleção de `janela.rs` faz.
    #[arg(long)]
    pub sem_som: bool,

    /// **Bancada.** Capturar o loopback de um endpoint que **não** é o padrão, pelo índice na
    /// lista de `--listar-saidas-de-audio`.
    ///
    /// Existe por privacidade, não por conveniência: o loopback da saída padrão captura a mistura
    /// da máquina inteira, e nenhum artefato dela pode ser gravado ou aberto. Um endpoint que não é
    /// o padrão só recebe o que este processo mandar para lá — é a única condição em que a origem é
    /// **provadamente** nossa e o lado que recebe pode conferir o artefato.
    #[arg(long)]
    pub saida_de_audio: Option<String>,

    /// **Bancada.** Toca um seno desta frequência no endpoint capturado, para o loopback ter uma
    /// origem sintética própria para achar. `1000` é o valor certo a 48 kHz: cai exatamente numa
    /// raia de 960 amostras. Nunca liga sozinho.
    #[arg(long)]
    pub tom_de_prova: Option<u32>,

    /// **Bancada.** Amplitude do tom, de 0 a 1.
    ///
    /// 0,03 é ~-30 dBFS: baixo o bastante para não incomodar quem está na sala com a máquina, e
    /// ainda umas mil vezes acima do piso de ruído numérico que o Goertzel tem de separar. **Nada
    /// nesta prova exige volume** — o que ela precisa é de uma frequência conhecida, não de energia.
    #[arg(long, default_value_t = 0.03)]
    pub tom_amplitude: f32,

    /// **Bancada.** Põe a track de áudio **antes** da de vídeo na oferta.
    ///
    /// Existe por uma limitação da sonda, e o que ela mede vale mais que o contorno:
    /// `quall-probe receber-audio` pega **a primeira track que chegar** e aborta se a espécie não
    /// bate. Com duas tracks na mesma sessão, medi que a `Screen` chegava primeiro — e a `Screen`
    /// era a primeira da oferta. Este sinalizador troca a ordem da oferta para responder se a
    /// ordem de chegada **segue** a da oferta ou é outra coisa. Nada no núcleo promete que siga:
    /// `session.rs` diz que o casamento é por `mid`, não por posição.
    #[arg(long)]
    pub som_primeiro: bool,

    /// **Bancada.** Volta a clicar em Espelhar toda vez que o app retorna à tela inicial.
    ///
    /// Existe para exercitar a **segunda sessão sem reiniciar o app** — que `docs/app-windows.md`
    /// listava como caminho de código escrito e nunca percorrido, e que é justamente onde a regra
    /// do `Ready` que vive numa thread só pode cobrar. Sem isto, cada corrida de bancada é um
    /// processo novo e a pergunta fica sem resposta.
    #[arg(long)]
    pub repetir_espelhar: bool,

    /// **Bancada.** Chama `esquecer_pares()` na partida e registra o antes e o depois.
    ///
    /// É a função que o botão "Esquecer pareamentos" chama. Não prova o **clique** — para isso
    /// falta um humano —, prova a função, que é a parte que pode estar quebrada em silêncio.
    #[arg(long)]
    pub esquecer_pareamentos: bool,

    /// **Bancada.** Lista os endpoints de saída ativos (com índice e id) e sai.
    #[arg(long)]
    pub listar_saidas_de_audio: bool,

    // --- o som do lado que exibe (S6, D1 e D3 do `docs/som-no-receptor.md` §12.1) ------------
    /// O receptor começa **mudo**. O padrão do produto é com som (D1): a pessoa cala na janela.
    /// Nasce mudo, o motor liga e puxa a porta do mesmo jeito, com o ganho em zero — é como as
    /// corridas de bancada rodam sem tocar nada no cômodo.
    #[arg(long)]
    pub mudo: bool,

    /// **Bancada: tocar mesmo com `--exibir-ja`.** O receptor aberto pela bancada (`--exibir-ja`)
    /// nasce **mudo**: os roteiros de receptor que já existiam (`prova-receptor.ps1`, os
    /// `tools/prova-*-windows.py`) não passam `--mudo`, e contra um emissor com som tocariam o som
    /// da máquina dele no cômodo do Dell (crítica 13, miúdo 13). Quem quer ouvir, como a prova da
    /// S6, pede isto de propósito. Sem `--exibir-ja` (o produto), não muda nada.
    #[arg(long)]
    pub som_na_bancada: bool,

    /// **Bancada: esquece o par deste `device_id` no `pares.json` e sai**, sem janela nem rede. É o
    /// passo de remoção da identidade fixa da sonda da prova da S6 (`probe-som-s6`).
    #[arg(long)]
    pub esquecer_par: Option<String>,

    /// O volume do receptor, de 0 a 1 (D1). A janela muda depois. NaN e infinito são recusados:
    /// passariam pelo `clamp` e iriam ao WASAPI (crítica 13, miúdo 6).
    #[arg(long, default_value_t = 1.0, value_parser = volume_de_0_a_1)]
    pub volume: f32,

    /// Tocar mesmo com a câmera do Quall em uso (D3). O padrão é calar enquanto algum app lê a
    /// câmera virtual de algum aparelho, para o som do cômodo não vazar para a chamada pelo
    /// microfone; a caixa da janela faz o mesmo que esta bandeira.
    #[arg(long)]
    pub som_com_camera: bool,

    /// **O emissor com várias sessões: o som não passa ao próximo** quando o receptor que o toca
    /// sai (D2). O padrão é passar, a sugestão que o Bruno aceitou; a confirmação está com o
    /// coordenador, e esta bandeira é o outro lado da escolha.
    #[arg(long)]
    pub sem_passar_som: bool,

    // --- o lado que exibe ---------------------------------------------------------------------
    /// **Bancada.** Clica em "Exibir" sozinho, contra este endereço (`192.168.1.131:7959`).
    ///
    /// O endereço digitado, e não a lista do mDNS, porque é o caminho que uma corrida
    /// automatizada consegue percorrer sem esperar um anúncio chegar — e porque o `PROMPT.md`
    /// fixa o fallback por IP como obrigatório, então ele é o caminho que **tem** de funcionar.
    #[arg(long)]
    pub exibir_ja: Option<String>,

    /// **Prende os candidatos ICE a UMA interface, pelo endereço local dela.**
    ///
    /// Existe porque, com o aparelho alcançável pelos dois caminhos, **o ICE escolhe o rádio**.
    /// Medido em 01/09/2026: o A10s ancorado por USB no Dell, sinalização apontada para o endereço
    /// do cabo (`192.168.152.57`), e a mídia saiu por `192.168.1.103 <-> 192.168.1.159` — LAN do
    /// Dell contra Wi-Fi do telefone. A corrida parecia "pelo cabo" e não era.
    ///
    /// **Ligar desiste das outras interfaces**: não há corrida entre cabo e rádio nem volta
    /// automática. É a diferença entre "prefere o cabo" e "só o cabo", e é o que obriga o produto
    /// a tratar cabo como escolha de quem usa.
    #[arg(long)]
    pub ligar_em: Option<String>,

    /// **Desliga a câmera virtual por aparelho pareado.**
    ///
    /// Nasce ligada porque é o produto que o usuário pediu: cada aparelho conhecido vira uma
    /// câmera na lista do Zoom, do Meet e do OBS, mostrando a placa de espera enquanto aquele
    /// aparelho não estiver transmitindo. A bandeira existe no **negativo** para que uma corrida
    /// possa medir o custo dela — a conversão para NV12 passa por um `Map` do contexto imediato,
    /// dentro do mesmo laço que apresenta na janela, e esse custo ainda não foi medido a 1080p.
    #[arg(long)]
    pub sem_cameras_virtuais: bool,

    /// **Bancada.** Volta a clicar em "Exibir" toda vez que o app retorna à tela inicial.
    ///
    /// O par de `--repetir-espelhar`, e pela mesma razão: é como se exercita uma **segunda
    /// sessão sem reiniciar o processo**, que é onde a regra do `Ready` numa thread só pode
    /// cobrar.
    #[arg(long)]
    pub repetir_exibir: bool,

    /// **Bancada.** Lê a régua de blocos do quadro decodificado e publica o número.
    ///
    /// **Só faz sentido quando a origem é a fonte sintética** (`gerar-fonte.swift`). Ela existe
    /// porque contador de quadro não sustenta a afirmação "exibiu vídeo": um decodificador pode
    /// entregar mil texturas cinzas com todos os contadores fechando. A régua responde por
    /// **inteiro**, nunca por pixel — ver `regua.rs` para por que ela é a única prova de pixel
    /// que esta frente pode dar sem furar a regra da casa.
    #[arg(long)]
    pub regua: bool,

    /// **Bancada** (a S7, T2 do `docs/som-no-receptor.md` §9.4): a claquete da sonda (`quall-probe
    /// emitir-video --claquete`). Liga a régua, guarda o índice de cada quadro apresentado com a
    /// hora do `Present` no QPC, procura o estouro de 3 150 Hz no som que sai (com a hora do DAC no
    /// QPC), e escreve as linhas `receptor claquete imagem` e `receptor claquete som` a cada
    /// segundo, com as capturas pelo relógio comum. Só faz sentido com a sonda.
    #[arg(long)]
    pub claquete: bool,

    /// Tamanho da janela de vídeo, como fração do tamanho do que chega. `1.0` é pixel a pixel.
    #[arg(long, default_value_t = 1.0)]
    pub escala_do_video: f64,

    /// **A porta que não entrega à tela um quadro cuja referência foi condenada.** Nasce
    /// **desligada**, e a flag mora no positivo por isso.
    ///
    /// Entre uma ruptura da cadeia e o IDR seguinte, todo quadro P chega inteiro, decodifica sem
    /// erro nenhum e sai visualmente podre. Com a porta ligada ele é decodificado assim mesmo —
    /// parar de alimentar o decodificador dessincroniza a sessão — e **não** é apresentado: a
    /// janela segura o último quadro bom, no máximo por 2 s (a válvula). Ver `crate::cadeia`.
    ///
    /// **Por que desligada.** Ligada por padrão ela foi mostrada ao usuário e a palavra dele foi
    /// *"terrível"*: a tela ficava parada, `fps 0,0`, e todos os intervalos batendo na válvula.
    /// Ela existe como sinalizador, e não como constante, porque esta bancada **mede** porta
    /// ligada contra desligada em vez de argumentar — e é o mesmo interruptor de A/B que o
    /// receptor iOS (`congelar`) e o plugin do OBS (a propriedade `congelar`) têm.
    ///
    /// Os contadores contam dos dois lados: `rupturas`, `suspeitos`, `pior_rajada` e
    /// `sem_referencia_ms` não dependem desta chave; só `retidos` depende. Virar a chave muda o que
    /// se vê, não o que se mede.
    #[arg(long)]
    pub congelar_na_ruptura: bool,

    /// **Bancada.** A câmera virtual como era em 09/09/2026: converte **todo** quadro apresentado,
    /// com ou sem alguém lendo a câmera, e colhe o quadro anterior **esperando** a GPU.
    ///
    /// O produto, desde 10/09, só converte quando há leitor no cano, no ritmo da câmera (30 fps),
    /// e colhe sem esperar. Este braço existe para o antes/depois ser medido **no mesmo binário**,
    /// intercalado — a revisão adversarial daquele dia lembrou que sem uma linha de base que
    /// reproduza o teto, conserto nenhum tem como mostrar ganho.
    #[arg(long)]
    pub camera_em_todo_quadro: bool,

    /// **Bancada.** Buffers da swap chain da janela de vídeo. Produto: 2.
    ///
    /// Com dois, um fica com o compositor até o retraço e o outro na fila — e o quadro seguinte
    /// pode ter de esperar o vsync do painel, com a GPU parada atrás dele. É um dos suspeitos do
    /// teto de 39 a 51 fps, e o A/B é trocar este número e mais nada.
    #[arg(long, default_value_t = 2)]
    pub buffers_da_janela: u32,

    /// **Bancada — um receptor lento de propósito.** O laço só tira da fila N quadros por segundo;
    /// o resto transborda, como num computador que não dá conta de decodificar.
    ///
    /// **É condição fabricada, e o nome diz isso.** Ela existe porque o gatilho `ReceptorAfogado`
    /// do controlador do emissor (`quall_core::taxa`) nunca disparou em aparelho: o Dell parou de
    /// afogar por outros consertos, e o S24 não manda 4K a 60. O que ela mede é o **controlador**
    /// diante de um receptor preso em fps — que é justamente o caso em que baixar bits não resolve
    /// —, e não este receptor.
    #[arg(long)]
    pub receptor_lento_fps: Option<u32>,

    /// **Bancada.** Entrega o SPS ao decoder exatamente como veio, sem declarar Constrained
    /// Baseline num fluxo Baseline.
    ///
    /// O produto, desde 10/09/2026, marca `constraint_set1_flag` no SPS Baseline — sem ela o
    /// decoder da Microsoft recusa DXVA e decodifica em software (7,4 ms por quadro contra 0,23 ms,
    /// medido com a câmera do S24). Ver `sps::declarar_constrained_baseline`. Desde 21/09/2026 (a S7
    /// do som) ele também declara a `bitstream_restriction` com `max_num_reorder_frames = 0` num SPS
    /// Baseline que não a declara (sem ela o decoder segura ~5 quadros: 161 ms de fila→tela). Ver
    /// `sps::declarar_restricao_de_bitstream`. Este é o braço de antes, dos dois.
    #[arg(long)]
    pub sps_como_veio: bool,

    /// **Bancada.** Abre o dispositivo D3D11 do receptor **sem** ligar a proteção multithread do
    /// contexto imediato — como era até 10/09/2026.
    ///
    /// O produto liga desde então, porque o decoder da Microsoft em DXVA usa o contexto de threads
    /// dele enquanto o laço da exibição o usa na thread da sessão. Ver `device::proteger_contexto`.
    /// Este é o braço de antes, para a queda do dispositivo (`0x887A0005`) ser reproduzida e
    /// comparada no mesmo binário.
    #[arg(long)]
    pub sem_protecao_multithread: bool,

    /// **Bancada — uma queda de GPU fabricada.** N segundos depois de cada abertura da exibição,
    /// ela passa a se comportar como se o dispositivo D3D11 tivesse caído.
    ///
    /// **É condição fabricada, e o nome diz isso.** Não há como derrubar um dispositivo D3D11 de
    /// propósito sem ferramenta de depuração instalada, e o caminho de reabrir
    /// (`receptor.rs`, "a GPU caiu") precisa ser provado antes de uma queda de verdade depender
    /// dele. O que ela exercita é o receptor largando e reabrindo — não o driver.
    #[arg(long)]
    pub simular_queda_da_gpu: Option<u64>,

    // --- o emissor com vários receptores ------------------------------------------------------
    /// **Bancada.** Várias sessões: uma thread e uma sessão por receptor, até 8; quando um
    /// receptor entra, a espera seguinte abre com porta e PIN novos, anunciada de novo. É o porte
    /// do emissor do Mac (`docs/tela-estendida.md`), a base do monitor estendido no Windows.
    ///
    /// **Atrás de bandeira porque o produto de hoje não tem para onde mandar cada receptor**: no Mac
    /// várias sessões só existem na tela estendida, e no Windows a tela estendida depende do driver
    /// de monitor virtual, que é outra frente. Sem a bandeira, o caminho é o de uma sessão só, que
    /// não foi tocado. Com ela e sem `--origem-sintetica`, cada sessão captura o monitor escolhido
    /// (provisório — ver `monitor.rs`).
    #[arg(long)]
    pub varias_sessoes: bool,

    /// **Bancada.** Com `--varias-sessoes`, cada sessão ganha uma **origem sintética** no formato da
    /// tela do receptor, em vez de capturar o monitor: `cor` (a tela numa cor que muda) ou `blocos`
    /// (blocos sorteados com uma faixa que anda). Nenhuma tela é capturada — e sem
    /// `Windows.Graphics.Capture` a cadeia roda também na Sessão 0 do SSH. Ver `sintetica.rs`.
    #[arg(long)]
    pub origem_sintetica: Option<crate::sintetica::Carga>,

    /// **Bancada.** Quando a origem sintética produz quadro: `movendo`, `parada` (um segundo e
    /// nada mais — a tela parada) ou `alterna:M/P` (M segundos sim, P não).
    #[arg(long, default_value = "movendo")]
    pub ritmo_sintetico: crate::sintetica::Ritmo,

    /// **Bancada.** Só MFT da Intel (Quick Sync), inclusive na reserva da troca a quente. É o
    /// encoder que o produto usa na sessão interativa; na Sessão 0 a ordem de produto sobe na
    /// NVIDIA, e a medida sairia do encoder errado (`docs/troca-a-quente.md`, achado 1).
    #[arg(long)]
    pub preferir_intel: bool,

    /// **Bancada.** Com `--varias-sessoes`, cada sessão ganha um **monitor virtual** do SudoVDA no
    /// formato da tela do receptor (`monitores_virtuais.rs`, `docs/monitor-virtual-windows.md` §14),
    /// em vez de capturar o monitor escolhido. O SudoVDA é só da bancada: o driver do produto é
    /// pergunta aberta ao usuário (§4.3). Na janela, o mesmo caminho é a fonte "Tela estendida".
    /// Sem `--varias-sessoes`, recusada (seria ignorada — a revisão de 15/09, item 16).
    #[arg(long, requires = "varias_sessoes")]
    #[cfg(feature = "tela-estendida-futura")]
    pub monitor_virtual: bool,

    /// **Bancada.** Cobre cada monitor virtual com a janela sintética nossa (`camadas`, `cor` ou
    /// `parada`) antes de capturar — sem ela o monitor mostra o papel de parede e o que a pessoa
    /// arrastar para lá (`cobertura.rs`). A captura só deixa passar quadro coberto. Só com
    /// `--varias-sessoes`.
    #[arg(long, requires = "varias_sessoes")]
    #[cfg(feature = "tela-estendida-futura")]
    pub cobrir_monitor: Option<crate::cobertura::Modo>,

    /// **Bancada.** A tela estendida escolhe a placa do monitor virtual **sem a Intel**
    /// (`regras_da_tela_estendida::placa_do_monitor`): é como provar no Dell o caminho de um
    /// computador sem Intel (só NVIDIA, só AMD). Vale para a fonte "Tela estendida" da janela e para
    /// `--monitor-virtual`, na primeira abertura do processo (a placa é uma por processo).
    ///
    /// **Cuidado no Dell**: se o H.264 da NVIDIA ativar e o `SET_RENDER_ADAPTER` for para ela, a
    /// próxima abertura normal (Intel) **trava o adaptador do SudoVDA até reiniciar**
    /// (`docs/monitor-virtual-windows.md` §12.3). Depois de uma corrida com esta bandeira, reiniciar
    /// o Dell antes de usar a tela estendida sem ela.
    #[arg(long)]
    #[cfg(feature = "tela-estendida-futura")]
    pub monitor_sem_intel: bool,

    /// **Bancada.** Com `--monitor-virtual`: o monitor nasce, a janela o cobre e desenha, mas a
    /// captura **nunca abre** — a sessão fica na origem preta. É o controle do custo da captura: o
    /// quanto as janelas desenham sem ninguém capturando (`docs/monitor-virtual-windows.md` §14). Só
    /// com `--varias-sessoes`.
    #[arg(long, requires = "varias_sessoes")]
    #[cfg(feature = "tela-estendida-futura")]
    pub monitor_sem_captura: bool,

    /// **Bancada.** A sinalização e o ICE só em `127.0.0.1`, e sem mDNS (nem o anúncio, nem a
    /// procura do receptor): emissor e receptores na mesma máquina, sem escutar fora dela. No Dell,
    /// um `.exe` novo que escuta fora do loopback na sessão interativa faz o firewall mostrar um
    /// aviso na tela do usuário. Vale nos dois caminhos: o de várias sessões, e o de uma sessão só
    /// desde a fase 4 da câmera (antes ela exigia `--varias-sessoes`, porque o caminho de uma sessão
    /// não a lia — a revisão de 15/09, item 16).
    #[arg(long)]
    pub so_local: bool,

    /// **Bancada.** O teto de quadro da tela estendida, em quadros médios da sessão (o do Mac é 5).
    /// `0` desliga — é o braço de controle da conferência no fluxo: a mesma origem com e sem teto,
    /// e o tamanho do IDR no `.h264` diz se o encoder obedeceu.
    #[arg(long, default_value_t = 5.0)]
    pub teto_quadro_em_medios: f64,

    // --- o teleprompter (as bandeiras do Mac, `apps/macos/Bancada/provar-teleprompter.sh`) ------
    /// **Bancada.** Abre **só** o teleprompter, direto no papel, sem a janela principal:
    /// `prompter` (mostra o texto, hospeda na 7979 com `--porta 0`) ou `controle`. Com `--porta`,
    /// `--pin`, `--sair-apos` e `--registro` de sempre. No produto a pessoa chega pelo botão
    /// "Teleprompter" da janela principal.
    /// `prompter-camera` abre a **tela R5** (o texto com a câmera, `docs/teleprompter-com-camera.md`
    /// §8.10), com as bandeiras `--r5-camera`, `--pin-da-camera` e as de bancada abaixo.
    #[arg(long, value_parser = ["prompter", "controle", "prompter-camera"])]
    pub teleprompter: Option<String>,

    // --- a tela R5 (bancada) -----------------------------------------------------------------
    /// **Bancada, R5.** A câmera da tela R5: o link, ou `sintetica` (a fonte do Quall neste processo,
    /// com a régua, sem câmera de ninguém). Sem a bandeira: a lembrada, a integrada, a primeira.
    #[arg(long)]
    pub r5_camera: Option<String>,

    /// **Bancada, R5.** O PIN fixo da sessão de vídeo da tela R5 (o do texto é `--pin`).
    #[arg(long)]
    pub pin_da_camera: Option<String>,

    /// **Bancada, R5.** A porta do vídeo da tela R5; `0` (o padrão) é a 7877.
    #[arg(long, default_value_t = 0)]
    pub porta_da_camera: u16,

    /// **Bancada, R5.** Liga o microfone S s depois de a câmera abrir, pelo caminho do botão. **Só com
    /// `--microfone-tom`** (`docs/audio.md` §8.1): sem ele, a tela recusa e diz.
    #[arg(long)]
    pub microfone_apos: Option<f64>,

    /// **Bancada, R5.** Desliga o microfone D s depois de ligar.
    #[arg(long)]
    pub microfone_por: Option<f64>,

    /// **Bancada.** O conteúdo de cada quadro do microfone vira um seno desta frequência **depois**
    /// do carimbo: o microfone abre (o ícone acende), o carimbo é o da captura, e o som da sala não sai
    /// do processo. Vale na tela R5 e na câmera comum.
    #[arg(long)]
    pub microfone_tom: Option<u32>,

    /// **Bancada, R5.** Começa a gravar S s depois de a câmera abrir, pelo caminho do botão.
    #[arg(long)]
    pub gravar_apos: Option<f64>,

    /// **Bancada, R5.** Para a gravação D s depois de ela começar.
    #[arg(long)]
    pub gravar_por: Option<f64>,

    /// **Bancada, R5.** `TerminateProcess` no próprio processo M s depois de a gravação começar (o
    /// órfão é recuperado na abertura seguinte da tela R5).
    #[arg(long)]
    pub matar_gravando_apos: Option<f64>,

    /// **Bancada, R5.** Esconde a prévia S s depois de a câmera abrir, por max(S, 30) s.
    #[arg(long)]
    pub esconder_previa_apos: Option<f64>,

    /// **Bancada.** A câmera comum pelo caminho de antes: a câmera nasce com a sessão (depois do
    /// pareamento), sem o dono e sem Gravar. O braço de controle da migração (§8.10.3).
    #[arg(long)]
    pub camera_comum_na_sessao: bool,

    /// **Bancada, R5.** A pasta das gravações, no lugar de `Vídeos\Quall`.
    #[arg(long)]
    pub pasta_das_gravacoes: Option<PathBuf>,

    // --- os controles de câmera do R9 (bancada, `docs/controles-de-camera.md` §5) -------------
    /// **Bancada, R9.** A média de luma de um quadro a cada 30 (`VideoProcessorBlt` para 64 × 36, o
    /// `Map` no quadro seguinte), com o custo, no diário. Nenhum quadro é salvo nem aberto.
    #[arg(long)]
    pub luma_media: bool,

    /// **Bancada, R9.** O roteiro dos ajustes da câmera, contado da câmera pronta para ajustes:
    /// `segundos:chave=valor,...;...` (`regras_dos_controles::ler_roteiro_de_bancada`). Cada passo
    /// deixa, ~3,5 s depois, uma linha `ajustes: medida` com o pedido, o `Get`, a luma e o fps. O
    /// registro parte do padrão e **nada é gravado** no `camera-ajustes.json`.
    #[arg(long, value_parser = roteiro_dos_ajustes)]
    pub ajustes_camera: Option<RoteiroDosAjustes>,

    /// **Bancada, R9.** No modo compartilhado, tenta um `Set` assim mesmo e diz o que voltou (a
    /// primeira medida da frente, §6). O produto deixa os controles apagados.
    #[arg(long)]
    pub ajustes_medir_compartilhada: bool,

    /// **Bancada, R9.** Uma linha por segundo com o `Get` de cada controle (o controlador do teste
    /// do modo compartilhado).
    #[arg(long)]
    pub ajustes_leitura: bool,

    /// **Bancada, R9.** Abre a janela "Ajustes da câmera" S s depois de a câmera abrir: a câmera
    /// comum (com a prévia dentro) ou a tela R5 (`--teleprompter prompter-camera`, sem prévia).
    #[arg(long)]
    pub abrir_ajustes_apos: Option<f64>,

    /// **Bancada, R5.** O vídeo da gravação no carimbo real, no lugar da grade de fps constante (o
    /// padrão desde a bancada de 27/09): o braço de comparação (`docs/teleprompter-com-camera.md`
    /// §8.10.6; com fps variável, o mux fMP4 do Media Foundation perdeu 0,67 s em 10 min).
    #[arg(long)]
    pub gravacao_no_carimbo_real: bool,

    // Um `quall://<pin>@host:porta` antigo ainda é aceito aqui (tolerância de entrada, em
    // `regras::destino_do_controle`), mas não se anuncia mais: nenhuma tela mostra o link.
    /// **Bancada.** O controle conecta sozinho neste prompter: `host` ou `host:porta`. IP sem porta
    /// completa com 7979 (a porta do teleprompter).
    #[arg(long)]
    pub prompter: Option<String>,

    /// **Bancada.** O roteiro inicial, lido deste arquivo (UTF-8) e aplicado como edição local.
    #[arg(long)]
    pub teleprompter_texto: Option<PathBuf>,

    /// **Bancada.** Edições programadas no tempo, pelo mesmo caminho dos botões:
    /// `segundos:campo=valor,...` (a gramática do Mac, mais `captura=arquivo.bmp`). Ver
    /// `teleprompter/regras.rs`, `AcoesDeBancada`.
    #[arg(long)]
    pub teleprompter_acoes: Option<String>,

    /// **Bancada.** Onde gravar o relato final em JSON: o resumo do roteiro, o estado final campo a
    /// campo, os contadores, a confirmação, a cadência da rolagem e o custo do layout.
    #[arg(long)]
    pub teleprompter_relato: Option<PathBuf>,

    /// **Bancada.** O tamanho da área de cliente da janela do teleprompter, em pixels: `1280x720`.
    #[arg(long)]
    pub teleprompter_janela: Option<String>,

    /// **Bancada.** O prompter não anuncia por mDNS: o controle tem de usar o endereço.
    #[arg(long)]
    pub sem_mdns: bool,

    /// **Bancada.** O prompter sem sessão nenhuma: só o roteiro rolando, sem abrir porta — para
    /// medir a rolagem sem rede (e sem o aviso do firewall por `.exe` novo).
    #[arg(long)]
    pub sem_sessao: bool,

    /// **Bancada.** A pasta de dados (`device-id.txt`, `pares.json`, os salvos do teleprompter):
    /// o mesmo que `QUALL_PASTA_DE_DADOS`. Dois processos na mesma máquina, com pastas diferentes,
    /// são dois aparelhos.
    #[arg(long)]
    pub dados: Option<PathBuf>,
}

impl std::fmt::Debug for Argumentos {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Argumentos").field("fps", &self.fps).field("porta", &self.porta)
            .field("so_local", &self.so_local).field("sem_som", &self.sem_som)
            .field("pin_informado", &self.pin.is_some()).field("fonte_informada", &self.fonte.is_some())
            .finish_non_exhaustive()
    }
}

impl Argumentos {
    pub fn lidos() -> Self {
        Argumentos::parse()
    }

    /// Quall Monitor nunca enumera as câmeras deste computador.
    pub fn com_cameras(&self) -> bool {
        false // Quall Monitor never enumerates or captures cameras.
    }

    /// Está rodando sob um agente, não sob uma pessoa?
    ///
    /// Governa uma coisa só: se o PIN pode ir para o arquivo de registro. Em modo de bancada ele
    /// foi **fixado por quem chamou** — não é segredo que este processo tenha sorteado.
    pub fn modo_de_bancada(&self) -> bool {
        self.espelhar_ja || self.sair_apos.is_some() || self.exibir_ja.is_some() || self.teleprompter.is_some()
    }

    /// A janela do teleprompter que as bandeiras pedem, se pedem uma.
    pub fn config_do_teleprompter(&self) -> Option<crate::teleprompter::ConfigDaTela> {
        let papel = match self.teleprompter.as_deref()? {
            "controle" => crate::teleprompter::Lado::Controle,
            _ => crate::teleprompter::Lado::Prompter,
        };
        let tamanho = self.teleprompter_janela.as_deref().and_then(|t| {
            let (w, h) = t.split_once(['x', 'X'])?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
        });
        Some(crate::teleprompter::ConfigDaTela {
            papel: Some(papel),
            porta: self.porta,
            pin: self.pin.clone(),
            prompter: self.prompter.clone(),
            sem_mdns: self.sem_mdns,
            sem_sessao: self.sem_sessao,
            texto: self.teleprompter_texto.clone(),
            acoes: self.teleprompter_acoes.clone(),
            relato: self.teleprompter_relato.clone(),
            sair_apos: self.sair_apos,
            tamanho,
            bancada: true,
            com_camera: false,
            r5: (self.teleprompter.as_deref() == Some("prompter-camera")).then(|| crate::teleprompter::ConfigDaTelaR5 {
                camera: self.r5_camera.clone(),
                pin_da_camera: self.pin_da_camera.clone(),
                porta_da_camera: self.porta_da_camera,
                so_local: self.so_local,
                microfone_apos: self.microfone_apos,
                microfone_por: self.microfone_por,
                microfone_tom: self.microfone_tom,
                gravar_apos: self.gravar_apos,
                gravar_por: self.gravar_por,
                matar_gravando_apos: self.matar_gravando_apos,
                esconder_previa_apos: self.esconder_previa_apos,
                pasta_das_gravacoes: self.pasta_das_gravacoes.clone(),
                abrir_ajustes_apos: self.abrir_ajustes_apos,
            }),
        })
    }

    /// A quinta porta está ligada? Padrão do produto: **sim**.
    ///
    /// O resto do código pergunta "está ligada?", não "está desligada?" — a flag é que mora no
    /// negativo, para o padrão poder ser ligado sem que quem lê o código precise inverter na
    /// cabeça a cada uso.
    pub fn idr_por_recriacao(&self) -> bool {
        !self.sem_idr_por_recriacao
    }

    /// A troca a quente está ligada? Padrão do produto desde 30/08: **sim**. Mesma convenção da
    /// quinta porta — a flag mora no negativo para o padrão poder ser ligado.
    pub fn troca_a_quente(&self) -> bool {
        !self.sem_troca_a_quente
    }

    /// A configuração da cadeia de áudio que estes argumentos descrevem.
    pub fn config_de_audio(&self) -> crate::audio::ConfigDeAudio {
        crate::audio::ConfigDeAudio {
            alvo: match self.saida_de_audio.as_deref() {
                Some(t) => crate::audio::Alvo::analisar(t),
                None => crate::audio::Alvo::Padrao,
            },
            tom_hz: self.tom_de_prova,
            tom_amplitude: self.tom_amplitude,
        }
    }
}

/// `--volume`: um número finito de 0 a 1.
fn volume_de_0_a_1(texto: &str) -> Result<f32, String> {
    match texto.parse::<f32>() {
        Ok(v) if v.is_finite() && (0.0..=1.0).contains(&v) => Ok(v),
        _ => Err(format!("um número de 0 a 1, e não {texto}")),
    }
}

impl Argumentos {
    /// O receptor nasce mudo? Pelo `--mudo`, ou pela bancada sem `--som-na-bancada`.
    pub fn receptor_nasce_mudo(&self) -> bool {
        self.mudo || (self.exibir_ja.is_some() && !self.som_na_bancada)
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn lidos(v: &[&str]) -> Result<Argumentos, clap::Error> {
        Argumentos::try_parse_from(std::iter::once("quall-app").chain(v.iter().copied()))
    }

    #[test]
    fn o_desentrelacador_de_bancada() {
        assert_eq!(lidos(&[]).unwrap().desentrelacador, "adapt2", "o padrão é o adapt2");
        assert_eq!(lidos(&["--desentrelacador", "bob"]).unwrap().desentrelacador, "bob");
        assert!(lidos(&["--desentrelacador", "weave"]).is_err());
        let a = lidos(&["--desentrelacador", "bob", "--sem-desentrelacar"]).unwrap();
        assert!(a.sem_desentrelacar && a.desentrelacador == "bob", "os dois convivem; o --sem-desentrelacar manda");
    }

    #[test]
    fn as_bandeiras_dos_ajustes_da_camera() {
        let a = lidos(&[]).unwrap();
        assert!(!a.luma_media && a.ajustes_camera.is_none() && !a.ajustes_medir_compartilhada, "o produto não liga nada");
        let a = lidos(&["--luma-media", "--ajustes-camera", "4:brilho=min;8:brilho=max", "--abrir-ajustes-apos", "3"]).unwrap();
        assert!(a.luma_media);
        assert_eq!(a.ajustes_camera.unwrap().0.len(), 2);
        assert_eq!(a.abrir_ajustes_apos, Some(3.0));
        assert!(lidos(&["--ajustes-camera", "4:zoom=max"]).is_err());
    }

    #[test]
    fn a_pausa_de_bancada_da_camera() {
        use std::time::Duration;
        assert_eq!(lidos(&[]).unwrap().pausar_camera, None, "o produto não pausa");
        let a = lidos(&["--camera-sintetica", "--pausar-camera", "20:15,60:40"]).unwrap();
        assert_eq!(
            a.pausar_camera,
            Some(PausasDeBancada(vec![(Duration::from_secs(20), Duration::from_secs(15)), (Duration::from_secs(60), Duration::from_secs(40))]))
        );
        assert!(lidos(&["--pausar-camera", "20"]).is_err());
    }

    /// **As câmeras entram no seletor por padrão** (a fase 5, com o sim do Bruno em 21/09). O
    /// `--cameras` das fases 1–4 é aceito e não muda nada (os roteiros de bancada antigos o passam);
    /// só o `--sem-cameras` (bancada) as tira, e ele não convive com a câmera de bancada.
    #[test]
    fn monitor_nunca_oferece_cameras() {
        assert!(!lidos(&[]).unwrap().com_cameras());
        assert!(!lidos(&["--cameras"]).unwrap().com_cameras());
        assert!(!lidos(&["--sem-cameras"]).unwrap().com_cameras());
        let link = r"\\?\swd#vcamdevapi#x";
        let a = lidos(&["--camera-de-bancada", link, "--fonte", link]).unwrap();
        assert_eq!(a.camera_de_bancada.as_deref(), Some(link), "a câmera de bancada não exige mais o --cameras");
        assert!(lidos(&["--sem-cameras", "--camera-de-bancada", link]).is_err(), "sem câmeras e a câmera de bancada se excluem");
    }
}

/// `--ajustes-camera`: o roteiro dos ajustes da câmera (R9).
#[derive(Clone, Debug, PartialEq)]
pub struct RoteiroDosAjustes(pub Vec<crate::regras_dos_controles::PassoDeBancada>);

fn roteiro_dos_ajustes(texto: &str) -> Result<RoteiroDosAjustes, String> {
    crate::regras_dos_controles::ler_roteiro_de_bancada(texto).map(RoteiroDosAjustes)
}

/// `--pausar-camera`: as janelas de pausa de bancada, `(início, duração)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PausasDeBancada(pub Vec<(std::time::Duration, std::time::Duration)>);

/// Lê `--pausar-camera` (`regras_da_camera::ler_pausas_de_bancada`).
fn pausas_de_bancada(texto: &str) -> Result<PausasDeBancada, String> {
    crate::regras_da_camera::ler_pausas_de_bancada(texto).map(PausasDeBancada)
}
