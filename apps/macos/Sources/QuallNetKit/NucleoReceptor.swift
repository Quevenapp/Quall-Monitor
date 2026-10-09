import CQuall
import Foundation
import QuallReceptorKit

/// A **outra metade** da ponte de rede: `quall_connect`, `quall_session_next_track`,
/// `quall_track_on_frame`, `quall_track_request_idr`.
///
/// `docs/app-macos.md` listou exatamente estas quatro funções como "a metade que falta em
/// `QuallNetKit`" quando o receptor foi cortado. `NucleoDeRede`, ao lado, é a metade que **emite**:
/// hospeda, manda quadro, lê a bandeira de IDR. Nenhuma das duas serve para o papel da outra —
/// `tracks` só vale em `quall_host`, e quem chama `quall_connect` nunca vai emitir naquela sessão
/// (dívida 1).
///
/// O desenho é o de `apps/ios/Receptor/Comum/NucleoReceptor.swift`, que foi escrito e provado em
/// aparelho em 2026-08-26, e por sua vez veio do receptor Android. Manter os três iguais é
/// deliberado: se um prazo estiver errado, é melhor que esteja errado nos três e o defeito apareça
/// em toda parte do que fique escondido numa plataforma só.
///
/// # As quatro decisões que este arquivo herda, e por que nenhuma é de estilo
///
/// 1. **O tratador de quadro é usado, e não uma bandeira.** No Android o header recomenda
///    `quall_track_take_idr_request` porque as threads da libdatachannel não estão anexadas à JVM.
///    Em Swift não há JVM: o tratador é um `@convention(c)` que só toca num objeto retido.
/// 2. **A caixa é liberada de verdade.** O receptor de câmera do macOS
///    (`integrations/camera-macos`) retém o `user_data` **para sempre**, e o próprio comentário
///    dele diz que isso "deixou de ser preço de segurança e virou vazamento puro" quando a dívida
///    24 foi paga. Aqui a barreira é usada como ela ficou: `quall_track_on_frame(t, NULL, NULL)`
///    primeiro, `quall_session_close(s)` depois, e a caixa só é liberada se **um dos dois**
///    devolver `QUALL_STATUS_OK`. Se nenhum devolver, ela vaza de propósito e o relato diz que
///    vazou.
/// 3. **A ramificação de falha é pelo código, não pelo texto.** `quall_last_status()` existe desde
///    2026-08-26 (dívida 27). O `NucleoDeRede` ao lado já segue essa regra; aqui não há nenhuma
///    comparação de texto. `QUALL_STATUS_NEEDS_PIN` é tratado como o header manda — convite a
///    mostrar a tela do PIN, não "falhou".
/// 4. **Nada de `quall_cleanup()`.** Mesma decisão do emissor (dívida 2): ele espera 10 s, desiste
///    e deixa uma thread de limpeza presa.
///
/// # Uma thread só toca a fronteira, e a interface nunca toca em handle
///
/// `quall_session_next_track` e `quall_session_next_event` **avançam estado** e o header exige que
/// venham de uma thread só. Aqui vêm as duas do mesmo laço. A interface tem exatamente um caminho
/// para interferir — [`parar()`] —, que levanta uma bandeira e, atrás de uma trava, aciona o
/// cancelador. Nenhum ponteiro de sessão ou de track sai desta classe.
public final class NucleoReceptor {

    /// O `user_data` do tratador de quadro. Um objeto retido cujo endereço atravessa a fronteira C.
    ///
    /// A bandeira `vivo` **não** é substituta da barreira — ela é o cinto de segurança para o caso
    /// em que a barreira falha (`QUALL_STATUS_TIMEOUT`, tratador que não voltou) e a caixa precisa
    /// continuar de pé. Com a barreira dando OK, ela nem chega a ser lida.
    final class Caixa {
        let aoQuadro: (UnsafeRawBufferPointer, UInt64, Bool) -> Void
        private let trava = NSLock()
        private var _vivo = true

        init(aoQuadro: @escaping (UnsafeRawBufferPointer, UInt64, Bool) -> Void) {
            self.aoQuadro = aoQuadro
        }

        var vivo: Bool {
            trava.lock(); defer { trava.unlock() }
            return _vivo
        }

        func desligar() { trava.lock(); _vivo = false; trava.unlock() }
    }

    // --- estado ------------------------------------------------------------------------------

    private let travaDoUso = NSLock()
    private var sessao: OpaquePointer?
    private var track: OpaquePointer?
    /// A track de som adotada, e a porta puxada dela. Ver `adotarProximaTrack` e `abrirSom`.
    private var trackDeAudio: OpaquePointer?
    private var audioAdotada: (rotulo: String, especie: EspecieDeTrack)?
    private var porta: PortaDeSom?
    private var caixa: Caixa?
    private var opacoDaCaixa: UnsafeMutableRawPointer?

    /// O cancelador vive numa trava própria e é zerado assim que a chamada bloqueante volta —
    /// depois disso `parar()` age só pela bandeira, e nunca com um handle já liberado.
    private let travaDoCancelador = NSLock()
    private var cancelador: OpaquePointer?

    private var vivas: [UnsafeMutablePointer<CChar>] = []

    private let travaDaParada = NSLock()
    private var _parar = false
    public var parou: Bool {
        travaDaParada.lock(); defer { travaDaParada.unlock() }
        return _parar
    }

    public private(set) var ultimoMotivo = ""
    public private(set) var ultimoStatus: QuallStatus = QUALL_STATUS_OK

    public init() {}

    // --- utilidade ---------------------------------------------------------------------------

    public static func ultimoErro() -> String {
        guard let p = quall_last_error() else { return "" }
        return String(cString: p)
    }

    /// Nome legível de um status, para o relato. **Sem `default` silencioso**: um código novo no
    /// núcleo aparece como número em vez de virar "desconhecido" e sumir.
    public static func nome(_ s: QuallStatus) -> String {
        switch s {
        case QUALL_STATUS_OK: return "OK"
        case QUALL_STATUS_INVALID: return "INVALID"
        case QUALL_STATUS_PROTOCOL: return "PROTOCOL"
        case QUALL_STATUS_DISCOVERY: return "DISCOVERY"
        case QUALL_STATUS_SIGNALING: return "SIGNALING"
        case QUALL_STATUS_TRANSPORT: return "TRANSPORT"
        case QUALL_STATUS_PAIRING: return "PAIRING"
        case QUALL_STATUS_TIMEOUT: return "TIMEOUT"
        case QUALL_STATUS_CLOSED: return "CLOSED"
        case QUALL_STATUS_IO: return "IO"
        case QUALL_STATUS_NULL_POINTER: return "NULL_POINTER"
        case QUALL_STATUS_NOT_UTF8: return "NOT_UTF8"
        case QUALL_STATUS_NO_ROUTE: return "NO_ROUTE"
        case QUALL_STATUS_NEEDS_PIN: return "NEEDS_PIN"
        case QUALL_STATUS_CANCELLED: return "CANCELLED"
        // Dívida 29, e ele **entrou depois** de o receptor iOS ser escrito: lá o PIN errado ainda
        // chega como `PAIRING`. Aqui ele tem nome porque este `switch` não tem `default`
        // silencioso — a primeira corrida de PIN errado desta bancada imprimiu `código 15`, que é
        // exatamente o que essa escolha existe para produzir.
        case QUALL_STATUS_WRONG_PIN: return "WRONG_PIN"
        // O "ocupado" do teleprompter (`docs/contrato-teleprompter.md` §2): um receptor de vídeo não
        // o recebe, mas sem o nome o registro mostraria `código 16`.
        case QUALL_STATUS_BUSY: return "BUSY"
        default: return "código \(s.rawValue)"
        }
    }

    private func guardar(_ texto: String) -> UnsafeMutablePointer<CChar> {
        let copia = strdup(texto) ?? UnsafeMutablePointer<CChar>.allocate(capacity: 1)
        vivas.append(copia)
        return copia
    }

    deinit {
        encerrar()
        for p in vivas { free(p) }
    }

    // --- conectar ----------------------------------------------------------------------------

    /// Conecta no emissor. **Bloqueia** até parear e o transporte subir, ou o prazo estourar.
    /// Chame de uma thread de trabalho.
    ///
    /// O cancelador é criado **antes** de largar a thread, como o header pede, para que [`parar()`]
    /// tenha o que acionar durante a espera inteira.
    /// `tela`: os pixels da tela deste Mac, ditos ao emissor no aperto de mão
    /// (`quall_connect_with_screen`). `(0, 0)` é "não digo".
    public func conectar(endereco: String,
                         pin: String?,
                         deviceId: String,
                         nome: String,
                         paresConhecidos: String?,
                         prazoMs: UInt32,
                         tela: (largura: UInt32, altura: UInt32) = (0, 0)) -> Bool {
        let idC = guardar(deviceId)
        let nomeC = guardar(nome)
        let enderecoC = guardar(endereco)
        let pinC: UnsafeMutablePointer<CChar>? = pin.map { guardar($0) }
        let paresC: UnsafeMutablePointer<CChar>? = paresConhecidos.map { guardar($0) }

        let novoCancelador = quall_canceller_new()
        travaDoCancelador.lock()
        cancelador = novoCancelador
        travaDoCancelador.unlock()
        // Stop may arrive before the session thread installs its canceller.
        if parou { quall_session_cancel(novoCancelador) }

        var opcoes = QuallSessionOptions(
            me: QuallDeviceDesc(device_id: idC,
                                display_name: nomeC,
                                // Este Mac, nesta sessão, é **sumidouro**: ele exibe, não
                                // transmite. É o oposto exato do que `NucleoDeRede.hospedar`
                                // declara na mesma máquina.
                                screen_source: false,
                                camera_source: false,
                                sink: true),
            pin: pinC.map { UnsafePointer($0) },
            known_peers_json: paresC.map { UnsafePointer($0) },
            // Só vale em `quall_host`. Um receptor não abre porta de sinalização.
            signaling_port: 0,
            timeout_ms: prazoMs,
            // Só vale em `quall_host`. As tracks que interessam aqui vêm do outro lado, por
            // `quall_session_next_track`.
            tracks: nil,
            track_count: 0,
            // **Este app não pede o cabo.** `nil` é o comportamento de sempre: o ICE reúne
            // todas as interfaces. Quem prende a mídia a uma delas é quem trata cabo como
            // escolha do usuário — hoje, só o plugin do OBS.
            bind_address: nil)

        // Fora de qualquer trava: a chamada bloqueia por dezenas de segundos e nada mais pode
        // ficar pendurado nela — inclusive o `parar()` da interface, que é quem a destrava.
        let nova = withUnsafePointer(to: &opcoes) {
            quall_connect_with_screen(enderecoC, $0, novoCancelador, tela.largura, tela.altura)
        }

        travaDoCancelador.lock()
        cancelador = nil
        travaDoCancelador.unlock()

        guard let nova else {
            // A regra de leitura do header: logo depois da chamada que falhou, antes de qualquer
            // outra função `quall_`, e só porque ela de fato devolveu nulo.
            ultimoStatus = quall_last_status()
            ultimoMotivo = NucleoReceptor.ultimoErro()
            quall_canceller_free(novoCancelador)
            return false
        }
        quall_canceller_free(novoCancelador)

        travaDoUso.lock()
        sessao = nova
        travaDoUso.unlock()
        return true
    }

    /// Levanta a bandeira e, se houver espera em curso, aciona o cancelador. Chamável de qualquer
    /// thread — e **não** toca em handle de sessão, de track nem na caixa.
    public func parar() {
        travaDaParada.lock(); _parar = true; travaDaParada.unlock()
        travaDoCancelador.lock()
        if let c = cancelador { quall_session_cancel(c) }
        travaDoCancelador.unlock()
    }

    // --- track -------------------------------------------------------------------------------

    /// Pega **uma** track e a arquiva pela espécie, e não pela ordem de chegada (S4 do
    /// `docs/som-no-receptor.md`, §7.0). É o desenho do receptor iOS (`NucleoReceptor.swift`).
    ///
    /// A ordem em que as tracks saem de `quall_session_next_track` não é contrato nenhum: tomar a
    /// primeira como vídeo deixa a imagem preta quando o som vem primeiro (crítica 2, M1). Vídeo é
    /// tela ou câmera; som é microfone ou som do sistema; o resto — a segunda de cada espécie, e
    /// espécie desconhecida — é largado e dito por `aoLargar`.
    ///
    /// `prazoMs` zero é uma espiada: uma chamada só, e volta. É como o laço de supervisão adota o
    /// som que chega depois do vídeo sem gastar volta nenhuma.
    @discardableResult
    public func adotarProximaTrack(prazoMs: UInt32,
                                   aoLargar: ((String) -> Void)? = nil)
        -> (rotulo: String, especie: EspecieDeTrack)? {
        travaDoUso.lock()
        let s = sessao
        travaDoUso.unlock()
        guard let s else { return nil }

        let limite = agoraUs() &+ UInt64(prazoMs) &* 1000
        var achada: OpaquePointer?
        repeat {
            if let t = quall_session_next_track(s, prazoMs == 0 ? 0 : 200) { achada = t; break }
        } while !parou && agoraUs() < limite
        guard let t = achada else { return nil }

        let rotulo = NucleoReceptor.rotulo(de: t)
        let especie = EspecieDeTrack(bruto: quall_track_kind(t).rawValue)
        travaDoUso.lock()
        let destino = destinoDaTrack(especie, jaTemVideo: track != nil, jaTemAudio: trackDeAudio != nil)
        switch destino {
        case .video:
            track = t
        case .audio:
            trackDeAudio = t
            audioAdotada = (rotulo, especie)
        case .largar:
            break
        }
        travaDoUso.unlock()
        if case .largar(let motivo) = destino {
            // Liberar é obrigatório: o handle é do chamador.
            quall_track_free(t)
            aoLargar?("\(motivo): \"\(rotulo)\"")
            return nil
        }
        return (rotulo, especie)
    }

    /// Espera a track de **vídeo**, arquivando pelo caminho a de som que chegar antes dela.
    ///
    /// Uma sessão pode trazer tela, câmera e som; esta rodada exibe **uma** de vídeo e toca **uma**
    /// de som. Se o som chega primeiro, o vídeo ainda tem **três segundos**, e não o prazo inteiro:
    /// as duas saem da mesma oferta SDP, e se a de vídeo existisse viria em milissegundos. É a regra
    /// do receptor iOS, medida lá em 30/08.
    public func esperarTrackDeVideo(prazoMs: UInt32,
                                    aoPular: ((String) -> Void)? = nil,
                                    aoAdotarSom: ((String, EspecieDeTrack) -> Void)? = nil)
        -> (rotulo: String, tipo: QuallTrackKind)? {
        var limite = agoraUs() &+ UInt64(prazoMs) &* 1000
        while !parou, agoraUs() < limite {
            guard let achada = adotarProximaTrack(prazoMs: 200, aoLargar: aoPular) else { continue }
            if achada.especie.eVideo {
                return (achada.rotulo, achada.especie == .tela ? QUALL_TRACK_KIND_SCREEN : QUALL_TRACK_KIND_CAMERA)
            }
            aoAdotarSom?(achada.rotulo, achada.especie)
            let curto = agoraUs() &+ 3_000_000
            if curto < limite { limite = curto }
        }
        return nil
    }

    // --- som ---------------------------------------------------------------------------------

    public var temTrackDeAudio: Bool {
        travaDoUso.lock(); defer { travaDoUso.unlock() }
        return trackDeAudio != nil
    }

    public var somAdotado: (rotulo: String, especie: EspecieDeTrack)? {
        travaDoUso.lock(); defer { travaDoUso.unlock() }
        return audioAdotada
    }

    /// Abre a porta puxada na track de som adotada. Uma vez; depois devolve a mesma.
    public func abrirSom() -> (porta: PortaDeSom?, motivo: String) {
        travaDoUso.lock()
        let t = trackDeAudio
        let ja = porta
        travaDoUso.unlock()
        if let ja { return (ja, "") }
        guard let t else { return (nil, "nenhuma track de som adotada") }
        let r = PortaDeSom.abrir(track: t)
        if let nova = r.porta {
            travaDoUso.lock(); porta = nova; travaDoUso.unlock()
        }
        return r
    }

    /// A porta de som aberta, se houver. Não abre nada.
    public var portaDeSom: PortaDeSom? {
        travaDoUso.lock(); defer { travaDoUso.unlock() }
        return porta
    }

    /// Libera a porta de som. **Só depois de o motor parar** (`PortaDeSom.encerrar`).
    @discardableResult
    public func encerrarSom() -> QuallStatus? {
        travaDoUso.lock()
        let p = porta
        porta = nil
        travaDoUso.unlock()
        return p?.encerrar()
    }

    /// Os contadores da track de som (`quall_track_stats_json`): o `clock` dela mora aqui.
    public func contadoresDoSom() -> String {
        travaDoUso.lock()
        let t = trackDeAudio
        travaDoUso.unlock()
        guard let t else { return "{}" }
        return NucleoDeRede.lerTexto { buf, cap in quall_track_stats_json(t, buf, cap) }
    }

    /// `timestamp_us + deslocamento` é a captura em µs desde a época da sessão, comum às tracks
    /// dela (`quall_track_capture_offset_us`). `nil` quando ainda não medido ou recusado.
    public func deslocamentoDeCaptura(doSom: Bool) -> Int64? {
        travaDoUso.lock()
        let t = doSom ? trackDeAudio : track
        travaDoUso.unlock()
        guard let t else { return nil }
        var us: Int64 = 0
        return quall_track_capture_offset_us(t, &us) == 1 ? us : nil
    }

    /// O rótulo da track, pelo padrão `(buf, cap)` — **pergunta o tamanho antes**.
    ///
    /// Era um buffer fixo de 256 bytes com a mesma leitura errada que `lerTexto` tinha: um
    /// retorno maior que a capacidade é positivo, e `n > 0` dava "deu certo" para um buffer que
    /// o núcleo não escreveu. O rótulo não é nosso — ele vem do SDP do emissor, e é "Tela do
    /// <nome do aparelho>" com o nome que uma pessoa digitou na outra ponta. 256 bytes é
    /// palpite; `lerTexto` pergunta.
    private static func rotulo(de t: OpaquePointer) -> String {
        NucleoDeRede.lerTexto { buf, cap in quall_track_label(t, buf, cap) }
    }

    /// Registra o tratador de quadro. **Roda numa thread da libdatachannel**: quem escreve o corpo
    /// dele não pode bloquear — bloquear ali segura a recepção da sessão inteira, e a barreira do
    /// `close` desiste depois de 2 s e devolve `QUALL_STATUS_TIMEOUT`.
    ///
    /// O `annexb` do quadro aponta para o buffer de remontagem do núcleo e vale **só durante a
    /// chamada**. O tratador desta casca entrega direto ao decodificador, sem copiar.
    public func ouvirQuadros(_ tratador: @escaping (UnsafeRawBufferPointer, UInt64, Bool) -> Void) -> QuallStatus {
        travaDoUso.lock()
        let t = track
        travaDoUso.unlock()
        guard let t else { return QUALL_STATUS_CLOSED }

        let nova = Caixa(aoQuadro: tratador)
        let opaco = Unmanaged.passRetained(nova).toOpaque()

        let estado = quall_track_on_frame(t, { quadro, dados in
            guard let quadro, let dados else { return }
            let caixa = Unmanaged<Caixa>.fromOpaque(dados).takeUnretainedValue()
            guard caixa.vivo else { return }
            let q = quadro.pointee
            guard let bytes = q.annexb, q.len > 0 else { return }
            caixa.aoQuadro(UnsafeRawBufferPointer(start: bytes, count: Int(q.len)),
                           q.timestamp_us, q.idr)
        }, opaco)

        if estado == QUALL_STATUS_OK {
            travaDoUso.lock()
            caixa = nova
            opacoDaCaixa = opaco
            travaDoUso.unlock()
        } else {
            // Nunca chegou a ser `user_data` de ninguém: pode ir embora agora, sem barreira.
            Unmanaged<Caixa>.fromOpaque(opaco).release()
        }
        return estado
    }

    /// `pedir_idr()` do contrato: emite PLI.
    ///
    /// Devolve erro enquanto a track não abriu, e o header diz que insistir por alguns
    /// milissegundos é o comportamento certo. Quem insiste é o laço; esta função só repassa o
    /// status, porque engolir o erro aqui seria reproduzir, do lado do receptor, o defeito que o
    /// contrato existe para evitar.
    @discardableResult
    public func pedirIdr() -> QuallStatus {
        travaDoUso.lock()
        let t = track
        travaDoUso.unlock()
        guard let t else { return QUALL_STATUS_CLOSED }
        return quall_track_request_idr(t)
    }

    /// Empresta o ponteiro da sessão de pé, atrás da trava (as mensagens da câmera, `CameraRemota.swift`).
    func comSessao<T>(_ corpo: (OpaquePointer) -> T?) -> T? {
        travaDoUso.lock(); defer { travaDoUso.unlock() }
        guard let sessao else { return nil }
        return corpo(sessao)
    }

    /// O detector de queda. Chame do **mesmo** laço do `esperarTrackDeVideo`, com prazo pequeno.
    public func evento(prazoMs: UInt32) -> QuallSessionEvent {
        travaDoUso.lock()
        let s = sessao
        travaDoUso.unlock()
        guard let s else { return QUALL_SESSION_EVENT_FAILED }
        return quall_session_next_event(s, prazoMs)
    }

    /// **O caminho de volta do sinal**: o receptor conta ao emissor o que viu do enlace numa
    /// janela. É o que faz o controlador de taxa do outro lado deixar de ser inerte.
    ///
    /// Os cinco números são **deltas da janela**, nunca acumulados desde o começo da sessão —
    /// quem faz essa conta é `JanelaDoEnlace`, em `QuallReceptorKit`. `pacotes` é o que o
    /// **emissor** mandou (`packets_seen + packets_lost_for_real`), e não o que chegou.
    ///
    /// # Uma thread só, e não é conselho
    ///
    /// O header exige que esta chamada saia da **mesma** thread que chama
    /// `quall_session_next_event`: as duas leem a sinalização, e a fronteira não põe cadeado no
    /// caminho quente para permitir o contrário. Nesta casca essa thread é a do laço de
    /// supervisão de `Receptor.exibir` — o `while !nucleo.parou` que já chama `evento(prazoMs:)`
    /// —, e é de lá, e só de lá, que ela é chamada.
    ///
    /// Falhar aqui não para nada: um emissor de versão anterior descarta a mensagem sozinho, e
    /// socket morto aparece no detector de queda que já existe. Por isso o status é **devolvido**
    /// em vez de tratado — quem decide quantas vezes registrar é o laço.
    public func relatarEnlace(ms: UInt64, pacotes: UInt64, perdidos: UInt64,
                              suspeitos: UInt64, idrsQuebrados: UInt64,
                              naoEntregues: UInt64 = 0) -> QuallStatus {
        travaDoUso.lock()
        let s = sessao
        travaDoUso.unlock()
        guard let s else { return QUALL_STATUS_CLOSED }
        return quall_session_report_link(
            s, ms, pacotes, perdidos, suspeitos, idrsQuebrados, naoEntregues)
    }

    // --- o que o núcleo sabe ------------------------------------------------------------------

    public var nomeDoPar: String {
        travaDoUso.lock()
        let s = sessao
        travaDoUso.unlock()
        guard let s else { return "" }
        let json = NucleoDeRede.lerTexto { buf, cap in quall_session_peer_json(s, buf, cap) }
        guard let dados = json.data(using: .utf8),
              let objeto = try? JSONSerialization.jsonObject(with: dados) as? [String: Any],
              let nome = objeto["display_name"] as? String
        else { return "" }
        return nome
    }

    public func pareamentoNovo() -> Bool {
        travaDoUso.lock()
        let s = sessao
        travaDoUso.unlock()
        guard let s else { return false }
        return quall_session_pairing_is_new(s)
    }

    public func paresConhecidos(somandoA base: String?) -> String {
        travaDoUso.lock()
        let s = sessao
        travaDoUso.unlock()
        guard let s else { return "" }
        if let base, !base.isEmpty {
            return base.withCString { antigos in
                NucleoDeRede.lerTexto { buf, cap in quall_session_known_peers_json(s, antigos, buf, cap) }
            }
        }
        return NucleoDeRede.lerTexto { buf, cap in quall_session_known_peers_json(s, nil, buf, cap) }
    }

    /// Quantos quadros o núcleo descartou por estarem incompletos — cada um é uma **ruptura da
    /// cadeia de referência**.
    ///
    /// Lido a cada volta do laço, e por isso não passa por JSON: o header separou esta função de
    /// `quall_track_stats_json` exatamente para que ninguém pagasse um alocador por volta e
    /// acabasse não perguntando. Foi assim que o receptor iOS ficou pedindo IDR **uma vez por
    /// sessão** enquanto a imagem se desfazia.
    public func quadrosDescartados() -> UInt64 {
        travaDoUso.lock()
        let t = track
        travaDoUso.unlock()
        guard let t else { return 0 }
        return quall_track_frames_dropped(t)
    }

    /// Contadores da track, como JSON. **Diferente do emissor, aqui dá para ler durante a
    /// corrida**: não existe `send_frame` deste lado, e o caminho do quadro é o tratador, que roda
    /// numa thread da libdatachannel e não passa por `travaDoUso` em lugar nenhum.
    public func contadores() -> String {
        travaDoUso.lock()
        let t = track
        travaDoUso.unlock()
        guard let t else { return "{}" }
        return NucleoDeRede.lerTexto { buf, cap in quall_track_stats_json(t, buf, cap) }
    }

    // --- desmonte ----------------------------------------------------------------------------

    /// Desmonta na ordem que o header exige, e libera a caixa **só** quando a barreira autoriza.
    ///
    /// Nunca chame de dentro do tratador de quadro: a fronteira recusa a espera com
    /// `QUALL_STATUS_INVALID` em vez de pendurar o processo, e a barreira não vale.
    ///
    /// Devolve o que aconteceu, para o relato poder dizer a verdade sobre a caixa.
    @discardableResult
    public func encerrar() -> (desregistro: QuallStatus?, fechamento: QuallStatus?, caixaLiberada: Bool) {
        // A porta de som antes de tudo. Quem a usa (o motor) já parou: é a regra de
        // `encerrarSom`, e o `Receptor` a cumpre antes de chamar aqui.
        encerrarSom()
        travaDoUso.lock()
        let t = track
        let ta = trackDeAudio
        let s = sessao
        let c = caixa
        let opaco = opacoDaCaixa
        track = nil
        trackDeAudio = nil
        audioAdotada = nil
        sessao = nil
        caixa = nil
        opacoDaCaixa = nil
        travaDoUso.unlock()

        guard t != nil || s != nil || ta != nil else { return (nil, nil, false) }

        // 1. Desligar o tratador **com barreira**. Só `OK` autoriza liberar a caixa. **Antes de
        //    soltar qualquer handle**, inclusive o do som: um tratador pode consultar outra track, e
        //    a ordem segura é todos os tratadores desregistrados antes de qualquer handle solto
        //    (`quall.h`, `quall_track_free`; crítica 9, miúdo 5).
        var desregistro: QuallStatus?
        if let t, opaco != nil {
            desregistro = quall_track_on_frame(t, nil, nil)
        }
        // O cinto de segurança, para o caso de a barreira ter falhado.
        c?.desligar()

        // 2. As tracks vão embora **antes** da sessão, como o header manda.
        if let ta { quall_track_free(ta) }
        if let t { quall_track_free(t) }

        // 3. `quall_session_close` também é barreira: se o desregistro falhou, ela ainda pode
        //    autorizar.
        var fechamento: QuallStatus?
        if let s { fechamento = quall_session_close(s) }

        var liberada = false
        if let opaco {
            if desregistro == QUALL_STATUS_OK || fechamento == QUALL_STATUS_OK {
                Unmanaged<Caixa>.fromOpaque(opaco).release()
                liberada = true
            }
            // Senão a caixa **vaza de propósito**: alguns bytes perdidos é preço baixo perto de um
            // tratador escrevendo em memória liberada. Quem relata diz que vazou.
        }
        // `quall_cleanup()` **não** é chamado: ver o cabeçalho desta classe.
        return (desregistro, fechamento, liberada)
    }

    private func agoraUs() -> UInt64 {
        var t = timespec()
        clock_gettime(CLOCK_UPTIME_RAW, &t)
        return UInt64(t.tv_sec) &* 1_000_000 &+ UInt64(t.tv_nsec) / 1_000
    }
}
