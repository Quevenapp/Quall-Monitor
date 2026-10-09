import CQuall
import Foundation

/// Ponte de rede entre o Swift do macOS e a fronteira C do núcleo — o que `apps/macos` nunca
/// teve até o M6 (a Frente 3 entregou só captura+encode, sem rede, de propósito).
///
/// O padrão de chamada é o mesmo que `apps/ios/Quall/Comum/Nucleo.swift` já provou em produto no
/// iPhone 7 (strings guardadas em campo até `quall_host` voltar, trava em volta dos dois
/// ponteiros da fronteira C, nunca `quall_cleanup()` — dívida 2). O que **não** veio de lá: nada
/// de bandeira de IDR, térmico ou App Group — isto é só o suficiente para hospedar uma sessão,
/// mandar quadros e fechar, porque o objetivo desta rodada é prová-lo compilando e rodando em
/// loopback no MacBook host, não entregar o app inteiro (ver `docs/ux-m6.md`, seção 4).
public final class NucleoDeRede {
    public static func versaoDoProtocolo() -> UInt16 { quall_protocol_version() }

    public static func ultimoErro() -> String {
        guard let ponteiro = quall_last_error() else { return "" }
        return String(cString: ponteiro)
    }

    /// PIN de seis dígitos, sorteado pelo núcleo — mesma razão do iOS: a qualidade do sorteio é
    /// o que segura o pareamento.
    ///
    /// **Buffer fixo aqui é julgamento, não descuido.** O PIN é de seis dígitos por contrato —
    /// 7 bytes com o NUL —, e passar por `lerTexto` seria *pior*: a chamada de sondagem
    /// sortearia um PIN e o jogaria fora, e a segunda sortearia outro. O que a regra exige e que
    /// faltava é a segunda metade da comparação: um retorno maior que a capacidade também é
    /// positivo, e sem `escritos <= buffer.count` ele passaria por sucesso com o buffer zerado.
    /// Ver `tools/confere-fronteira.py`, PERMITIDOS.
    public static func sortearPin() -> String {
        var buffer = [CChar](repeating: 0, count: 16)
        let escritos = buffer.withUnsafeMutableBufferPointer { p -> Int in
            Int(quall_generate_pin(p.baseAddress, UInt(p.count)))
        }
        guard escritos > 0, escritos <= buffer.count else { return "" }
        return String(cString: buffer)
    }

    /// O que `quall_teto_ajustar_para` respondeu, sem tipo da fronteira C vazando para quem chama.
    public struct Teto: Sendable, Equatable {
        public let largura: Int
        public let altura: Int
        public let fps: Int32
        public let macroblocos: Int
        public let maxFS: Int
        public let tetoDeTaxaBps: Int
        public let levelIdc: Int
        public let reduziuTamanho: Bool
        public let reduziuFps: Bool
        public let exigeRecorte: Bool
    }

    /// **Quanto deste quadro pode ir para a rede, perguntado ao núcleo** — com a resolução e a taxa
    /// que quem chama escolheu, e cortado pelo nível que o SDP deste binário anuncia.
    ///
    /// É o que a cópia local `TetoDoEmissor` diz que falta ("o acabamento certo é consultar a
    /// fronteira C"). Hoje só a tela estendida pergunta por aqui: as outras fontes continuam na
    /// cópia, e trocá-las mudaria a geometria de corridas já medidas — é outra entrega.
    ///
    /// `nil` quando a fronteira devolve erro; quem chama diz isso no registro em vez de inventar.
    public static func teto(largura: Int, altura: Int, fps: Int32,
                            alvoMaxFs: Int, alvoFps: Int32) -> Teto? {
        var saida = QuallTeto()
        let status = quall_teto_ajustar_para(
            UInt32(max(0, largura)), UInt32(max(0, altura)), UInt32(max(0, fps)),
            UInt32(max(0, alvoMaxFs)), UInt32(max(0, alvoFps)), &saida)
        guard status == QUALL_STATUS_OK else { return nil }
        return Teto(largura: Int(saida.largura), altura: Int(saida.altura), fps: Int32(saida.fps),
                    macroblocos: Int(saida.macroblocos), maxFS: Int(saida.max_fs),
                    tetoDeTaxaBps: Int(saida.teto_de_taxa_bps), levelIdc: Int(saida.level_idc),
                    reduziuTamanho: saida.reduziu_tamanho != 0, reduziuFps: saida.reduziu_fps != 0,
                    exigeRecorte: saida.exige_recorte != 0)
    }

    /// Padrão `(buf, cap)` da fronteira C: **pergunta o tamanho com `buf` nulo, aloca, chama de
    /// novo.**
    ///
    /// # O defeito que esta função tinha, e o que ele custou em silêncio
    ///
    /// Achado em 2026-08-30, pela frente do receptor, quando um pareamento que tinha acabado de
    /// fechar não foi retomado na corrida seguinte. A versão anterior tentava com 256 bytes e
    /// crescia **quando o retorno fosse negativo**:
    ///
    /// ```swift
    /// if escritos >= 0 { return String(cString: buffer) }   // <- aqui
    /// cap *= 4
    /// ```
    ///
    /// O contrato do header, no topo de `quall.h`, diz outra coisa: *"a função devolve quantos
    /// bytes são necessários incluindo o NUL, **e só escreve se couber**. Negativo é erro"*. Buffer
    /// pequeno demais **não é negativo** — é positivo, é o tamanho necessário, e a função não
    /// escreve nada. Então o laço nunca crescia: ele lia `escritos >= 0`, devolvia
    /// `String(cString:)` de um buffer todo zerado, e o resultado era **string vazia**.
    ///
    /// Efeito, e ele é de produto: **qualquer resposta maior que 256 bytes virava `""`.** A pior é
    /// `quall_session_known_peers_json` — o estado de pareamento. `Identidade.guardarPares`
    /// desiste com string vazia, então **o pareamento parou de ser gravado no dia em que o
    /// `pares.json` passou de 256 bytes**, que é exatamente o defeito que o pareamento existe para
    /// evitar: "funcionou ontem, hoje pede o PIN de novo". O arquivo desta bancada tem 10 KB e
    /// data de **26/08**; toda corrida do app depois disso achou que gravou e não gravou.
    ///
    /// **O alcance, medido e não suposto.** As outras respostas desta fronteira ainda cabiam em
    /// 256 bytes nesta bancada — `quall_track_stats_json` do emissor deu 98 bytes, o do receptor
    /// ~230, e `quall_session_peer_json` ~190. Elas passavam. A que quebrou é a **única que cresce
    /// sem teto**: o estado de pareamento cresce um par por aparelho, para sempre. Ou seja, o
    /// defeito não se manifestava no dia em que foi escrito — ele esperava o arquivo engordar, e
    /// as duas outras estão a poucas dezenas de bytes de cair no mesmo buraco (o do receptor
    /// passaria de 256 com um contador novo).
    ///
    /// Nada disso dava erro em lugar nenhum — é a família de defeito que este projeto já mediu
    /// três vezes: a chamada "aceita", o contador sobe, e o que falta é invisível.
    static func lerTexto(_ chamada: (UnsafeMutablePointer<CChar>?, UInt) -> Int) -> String {
        let precisa = chamada(nil, 0)
        guard precisa > 0 else { return "" }
        var buffer = [CChar](repeating: 0, count: precisa)
        let escritos = buffer.withUnsafeMutableBufferPointer { p in
            chamada(p.baseAddress, UInt(p.count))
        }
        // `escritos <= precisa` não é redundante com o `guard` de cima: `escritos > 0` sozinho é
        // **exatamente a leitura errada** que custou quatro dias de pareamento, e esta função é
        // de onde a próxima casca vai copiar o padrão. Escrever a condição inteira aqui é o que
        // impede a cópia de nascer errada.
        guard escritos > 0, escritos <= precisa else { return "" }
        return String(cString: buffer)
    }

    /// Uma track pedida na abertura da sessão.
    ///
    /// `tracks` só vale em `quall_host` e **não há renegociação** (dívida 1): a lista inteira é
    /// decidida antes de o primeiro aparelho conectar, e acrescentar som depois exigiria derrubar
    /// a sessão. É por isso que a escolha "com ou sem som" é feita na tela inicial, junto da
    /// origem, e não durante a transmissão.
    public struct DescricaoDeTrack: Sendable {
        public let tipo: QuallTrackKind
        public let rotulo: String
        /// **O codec que o núcleo vai anunciar no SDP.** `QUALL_AUDIO_CODEC_DEFAULT` deixa a
        /// espécie decidir (Opus, para áudio de sistema).
        ///
        /// Pedir explicitamente não é preferência: é a única forma de o `a=rtpmap` dizer a
        /// verdade. Medido antes de este campo existir, a track de som negociava
        /// `opus/48000/2` porque era o padrão da espécie — e esta casca só sabe produzir G.711.
        /// Mandar µ-law ali anunciaria Opus com payload de tabela de consulta.
        public let codecDeAudio: QuallAudioCodec

        public init(tipo: QuallTrackKind, rotulo: String,
                    codecDeAudio: QuallAudioCodec = QUALL_AUDIO_CODEC_DEFAULT) {
            self.tipo = tipo
            self.rotulo = rotulo
            self.codecDeAudio = codecDeAudio
        }
    }

    private let travaDoUso = NSLock()
    private var sessao: OpaquePointer?
    /// As tracks de saída, **na ordem em que foram pedidas** — que é a ordem que
    /// `quall_session_track(s, idx)` promete. Guardar a ordem é o que permite dizer "a de índice 0
    /// é o vídeo, a 1 é o som" sem perguntar a espécie de volta ao núcleo a cada quadro.
    private var tracks: [OpaquePointer] = []
    private var indiceDeVideo: Int?
    private var indiceDeAudio: Int?
    private var vivas: [UnsafeMutablePointer<CChar>] = []

    public private(set) var ultimoMotivo = ""
    /// O status que o núcleo devolveu na última falha de `hospedar`. Ver o comentário lá dentro:
    /// é ele, e não o texto da mensagem, que decide o que a interface oferece a seguir.
    public private(set) var ultimoStatusDeFalha = QUALL_STATUS_OK

    public init() {}

    deinit {
        encerrar()
        for p in vivas { free(p) }
    }

    private func guardar(_ texto: String) -> UnsafeMutablePointer<CChar> {
        let copia = strdup(texto) ?? UnsafeMutablePointer<CChar>.allocate(capacity: 1)
        vivas.append(copia)
        return copia
    }

    /// Sobe uma sessão de emissor com as tracks pedidas. Bloqueia até o receptor chegar, parear e
    /// o transporte subir, ou até o prazo estourar. Chame de uma thread de trabalho.
    ///
    /// **Era de uma track só até 2026-08-27.** Passou a ser uma lista porque o `PROMPT.md` promete
    /// que "uma sessão carrega múltiplas tracks" e o áudio de sistema é a primeira vez que o
    /// produto exerce isso: tela e som atravessam a mesma sessão, o mesmo transporte DTLS-SRTP e o
    /// mesmo par de candidatos ICE, cada uma com sua linha `m=` (`docs/audio.md` §5).
    public func hospedar(
        pin: String,
        porta: UInt16,
        deviceId: String,
        nome: String,
        tracksPedidas: [DescricaoDeTrack],
        paresConhecidos: String?,
        prazoMs: UInt32,
        cancelador: Cancelador? = nil,
        ligarEm: String? = nil
    ) -> Bool {
        guard !tracksPedidas.isEmpty else {
            ultimoMotivo = "hospedar sem track nenhuma"
            return false
        }
        let idC = guardar(deviceId)
        let nomeC = guardar(nome)
        let pinC = guardar(pin)
        let paresC: UnsafeMutablePointer<CChar>? = paresConhecidos.map { guardar($0) }
        let ligarC: UnsafeMutablePointer<CChar>? = ligarEm.map { guardar($0) }

        // Os rótulos precisam continuar vivos **durante a chamada** — o header exige isso das
        // strings de `QuallSessionOptions`. `guardar` os põe em `vivas`, que só é liberado no
        // `deinit`; um `withCString` aninhado por track daria a mesma garantia com uma pirâmide de
        // closures que cresce com o número de tracks.
        let descricoes = tracksPedidas.map {
            QuallTrackDesc(kind: $0.tipo, label: UnsafePointer(guardar($0.rotulo)),
                           audio_codec: $0.codecDeAudio)
        }
        let temTela = tracksPedidas.contains { $0.tipo == QUALL_TRACK_KIND_SCREEN }
        let temCamera = tracksPedidas.contains { $0.tipo == QUALL_TRACK_KIND_CAMERA }

        return descricoes.withUnsafeBufferPointer { buffer -> Bool in
            var opcoes = QuallSessionOptions(
                me: QuallDeviceDesc(
                    device_id: UnsafePointer(idC),
                    display_name: UnsafePointer(nomeC),
                    screen_source: temTela,
                    camera_source: temCamera,
                    sink: false),
                pin: UnsafePointer(pinC),
                known_peers_json: paresC.map { UnsafePointer($0) },
                signaling_port: porta,
                timeout_ms: prazoMs,
                tracks: buffer.baseAddress,
                track_count: UInt(buffer.count),
                // `nil` é o comportamento de sempre: o ICE reúne todas as interfaces. Um endereço
                // prende a mídia **àquela** interface e desiste das outras — é a escolha "Sair pela
                // rede" da tela inicial, feita pela pessoa (`docs/fluxo-de-uso.md` §3: escolher tem de
                // desligar o outro caminho, não só preferir um).
                bind_address: ligarC.map { UnsafePointer($0) })

            // `quall_host_cancelable` com cancelador nulo reproduz `quall_host` exatamente — o
            // header promete isso, e é o que deixa o binário de bancada seguir sem cancelador
            // enquanto o app de produto ganha um botão Cancelar que funciona.
            let nova = withUnsafePointer(to: &opcoes) { p -> OpaquePointer? in
                guard let cancelador else { return quall_host(p) }
                return cancelador.comPonteiro { c in quall_host_cancelable(p, c) }
            }
            guard let nova else {
                // **O status vem antes da mensagem.** A casca iOS já pagou por comparar prefixo
                // de texto em português para descobrir que o ICE não achou caminho — quebra na
                // primeira vez que alguém reescreve a frase. `QUALL_STATUS_NEEDS_PIN`,
                // `_CANCELLED` e `_NO_ROUTE` mudam o que a tela oferece, não só o que ela diz.
                ultimoStatusDeFalha = quall_last_status()
                ultimoMotivo = NucleoDeRede.ultimoErro()
                return false
            }
            // **Conferir a contagem, e não confiar nela.** Uma sessão que subiu com menos tracks
            // do que foram pedidas transmitiria o vídeo e engoliria o som em silêncio — os
            // contadores de áudio ficariam em zero sem ninguém saber por quê.
            let quantas = Int(quall_session_track_count(nova))
            guard quantas == descricoes.count else {
                ultimoMotivo = "pedi \(descricoes.count) track(s) e a sessão subiu com \(quantas)"
                quall_session_close(nova)
                return false
            }
            var abertas: [OpaquePointer] = []
            for i in 0..<quantas {
                guard let t = quall_session_track(nova, UInt(i)) else {
                    ultimoMotivo = "a track de índice \(i) veio nula"
                    for t in abertas { quall_track_free(t) }
                    quall_session_close(nova)
                    return false
                }
                abertas.append(t)
            }

            travaDoUso.lock()
            sessao = nova
            tracks = abertas
            // O índice vem da **ordem pedida**, não de uma varredura por espécie: é a ordem que o
            // header promete em `quall_session_track`, e é a mesma lista que montou o SDP.
            indiceDeVideo = tracksPedidas.firstIndex {
                $0.tipo == QUALL_TRACK_KIND_SCREEN || $0.tipo == QUALL_TRACK_KIND_CAMERA
            }
            // Som é o do sistema **ou o do microfone** (R5 fase 4): a sessão de câmera leva a track
            // de microfone sempre na oferta, e é por ela que o som da câmera sai.
            indiceDeAudio = tracksPedidas.firstIndex {
                $0.tipo == QUALL_TRACK_KIND_SYSTEM_AUDIO || $0.tipo == QUALL_TRACK_KIND_MICROPHONE
            }
            travaDoUso.unlock()
            return true
        }
    }

    /// Empresta o ponteiro da sessão de pé, atrás da trava (as mensagens da câmera, `CameraRemota.swift`).
    func comSessao<T>(_ corpo: (OpaquePointer) -> T?) -> T? {
        travaDoUso.lock(); defer { travaDoUso.unlock() }
        guard let sessao else { return nil }
        return corpo(sessao)
    }

    public var porta: UInt16 {
        travaDoUso.lock(); defer { travaDoUso.unlock() }
        guard let sessao else { return 0 }
        return quall_session_signaling_port(sessao)
    }

    public var pareamentoNovo: Bool {
        travaDoUso.lock(); defer { travaDoUso.unlock() }
        guard let sessao else { return true }
        return quall_session_pairing_is_new(sessao)
    }

    /// Quantos candidatos caíram antes deste, sem derrubar a espera. `0` é o caso normal.
    ///
    /// Diferente de zero quer dizer que alguém conectou e sumiu no meio. Até 01/09/2026 isso
    /// fechava a porta da sinalização em definitivo, com o processo vivo — o "depois que sai não
    /// conecta". Ver `quall_session_descartados`.
    public var candidatosDescartados: Int {
        travaDoUso.lock(); defer { travaDoUso.unlock() }
        guard let sessao else { return 0 }
        return Int(quall_session_descartados(sessao))
    }

    public func paresParaGuardar(somandoA conhecidos: String?) -> String {
        travaDoUso.lock()
        let s = sessao
        travaDoUso.unlock()
        guard let s else { return "" }
        if let conhecidos, !conhecidos.isEmpty {
            return conhecidos.withCString { antigos in
                NucleoDeRede.lerTexto { buf, cap in quall_session_known_peers_json(s, antigos, buf, cap) }
            }
        }
        return NucleoDeRede.lerTexto { buf, cap in quall_session_known_peers_json(s, nil, buf, cap) }
    }

    /// `enviar_quadro` do contrato: empacota e solta. O buffer é do chamador; o núcleo não o
    /// guarda.
    @discardableResult
    public func enviar(annexb: UnsafeRawBufferPointer, timestampUs: UInt64, idr: Bool) -> QuallStatus {
        travaDoUso.lock(); defer { travaDoUso.unlock() }
        guard let i = indiceDeVideo, i < tracks.count, let base = annexb.baseAddress else {
            return QUALL_STATUS_INVALID
        }
        var quadro = QuallFrame(
            annexb: base.assumingMemoryBound(to: UInt8.self),
            len: UInt(annexb.count),
            timestamp_us: timestampUs,
            idr: idr)
        return withUnsafePointer(to: &quadro) { quall_track_send_frame(tracks[i], $0) }
    }

    /// Esta sessão abriu uma track de áudio de sistema?
    public var temTrackDeAudio: Bool {
        travaDoUso.lock(); defer { travaDoUso.unlock() }
        return indiceDeAudio != nil
    }

    /// `enviar_audio` do contrato: **um quadro codificado por chamada**.
    ///
    /// `docs/audio.md` §6: o `AudioRtpPacketizer` da libdatachannel não fragmenta — uma mensagem
    /// entra, um pacote RTP sai. Dois quadros de Opus concatenados numa chamada viram um pacote
    /// que o outro lado decodifica errado, e **sem erro nenhum no caminho**, porque para o RTP é
    /// só um payload maior. Quem garante o fatiamento é `CodificadorDeAudio`, do lado da captura.
    ///
    /// Erro aqui é a track ainda não estar aberta — estado normal enquanto o ICE não fechou — ou
    /// o transporte ter caído. Como no vídeo, a casca **descarta o quadro e segue**: enfileirar
    /// áudio ao vivo para tentar de novo entrega um estalo tarde em vez de um buraco na hora.
    @discardableResult
    public func enviarAudio(quadro: UnsafeRawBufferPointer, timestampUs: UInt64) -> QuallStatus {
        travaDoUso.lock(); defer { travaDoUso.unlock() }
        guard let i = indiceDeAudio, i < tracks.count, let base = quadro.baseAddress else {
            return QUALL_STATUS_INVALID
        }
        var amostra = QuallAudioSample(
            payload: base.assumingMemoryBound(to: UInt8.self),
            len: UInt(quadro.count),
            timestamp_us: timestampUs)
        return withUnsafePointer(to: &amostra) { quall_track_send_audio(tracks[i], $0) }
    }

    /// **O detector de queda** (dívidas 5, 13 e 20).
    ///
    /// Sem ele o único sinal de que a sessão morreu é `enviar` voltar a falhar — e esse mesmo
    /// status é o estado **normal** enquanto o ICE ainda não fechou. Pior: o `CONSENT_TIMEOUT` do
    /// libjuice é 30 000 ms, então por meio minuto o envio continua devolvendo sucesso com o
    /// receptor morto: ~900 quadros capturados, codificados em hardware e empacotados para o
    /// vazio. O `Bye` que o receptor manda ao sair já chega ao emissor; isto é o que o lê.
    ///
    /// # Uma thread só
    ///
    /// O header exige: esta função lê da sinalização e pede `*mut`. Chame de **uma** thread.
    /// O prazo aqui é `0` de propósito — a trava desta classe também protege `enviar`, e um
    /// prazo de 10 ms seguraria o caminho do quadro por 10 ms a cada sondagem. Quem espera é o
    /// laço de supervisão da casca, do lado de fora da trava.
    public func proximoEvento() -> QuallSessionEvent {
        travaDoUso.lock(); defer { travaDoUso.unlock() }
        guard let sessao else { return QUALL_SESSION_EVENT_NONE }
        return quall_session_next_event(sessao, 0)
    }

    /// O receptor pediu um quadro-chave? Consome a bandeira (`quall_track_take_idr_request`).
    ///
    /// Preferida ao callback pelo mesmo motivo que o Android prefere: a casca já tem um laço por
    /// quadro, e uma leitura atômica por quadro não custa nada, enquanto o tratador rodaria numa
    /// thread da libdatachannel.
    public func precisaDeIDR() -> Bool {
        travaDoUso.lock(); defer { travaDoUso.unlock() }
        guard let i = indiceDeVideo, i < tracks.count else { return false }
        return quall_track_take_idr_request(tracks[i])
    }

    /// JSON do par conectado — a casca usa o nome dele na tela ("Espelhando PARA …").
    public func parJson() -> String {
        travaDoUso.lock()
        let s = sessao
        travaDoUso.unlock()
        guard let s else { return "" }
        return NucleoDeRede.lerTexto { buf, cap in quall_session_peer_json(s, buf, cap) }
    }

    /// Contadores da track, como JSON: `frames_sent`, `idrs_sent`, `idr_requests`,
    /// `buffered_bytes`. É o que se relata em vez de olhar pixel.
    public func estatisticasDaTrack() -> String {
        travaDoUso.lock()
        let i = indiceDeVideo
        let quais = tracks
        travaDoUso.unlock()
        guard let i, i < quais.count else { return "" }
        return NucleoDeRede.lerTexto { buf, cap in quall_track_stats_json(quais[i], buf, cap) }
    }

    /// Os contadores da track de **áudio**, como JSON. Vazio quando a sessão não tem som.
    ///
    /// Relatado separado do vídeo de propósito: são duas tracks, com espaços de sequência RTP
    /// próprios e contadores próprios (`docs/audio.md` §5). Somá-los esconderia justamente a
    /// pergunta que esta rodada existe para responder — o som atravessou junto da imagem?
    public func estatisticasDaTrackDeAudio() -> String {
        travaDoUso.lock()
        let i = indiceDeAudio
        let quais = tracks
        travaDoUso.unlock()
        guard let i, i < quais.count else { return "" }
        return NucleoDeRede.lerTexto { buf, cap in quall_track_stats_json(quais[i], buf, cap) }
    }

    /// **Esquece um par** — o que a casca oferece como "parear de novo" (dívida 22).
    ///
    /// Sem isto, um pareamento que dessincronizou não tem saída: o `Resume` morre em "não está
    /// pareado aqui", o produto não oferece digitar o PIN outra vez, e o usuário vive o pior tipo
    /// de defeito — funcionou ontem, hoje não funciona.
    public static func esquecerPar(paresJson: String, deviceId: String) -> String {
        paresJson.withCString { pares in
            deviceId.withCString { id in
                lerTexto { buf, cap in quall_known_peers_forget(pares, id, buf, cap) }
            }
        }
    }

    /// **Funde dois estados de pareamento** (dívida 23). União; na colisão vence o mais recente.
    ///
    /// A casca lê o disco, funde com o que tem na mão e grava — assim uma corrida perdida deixa
    /// de apagar uma entrada e passa a convergir para a mais recente. Não dispensa a trava entre
    /// processos; reduz o estrago de quando ela falhar.
    public static func fundirPares(_ a: String, _ b: String) -> String {
        a.withCString { pa in
            b.withCString { pb in
                lerTexto { buf, cap in quall_known_peers_merge(pa, pb, buf, cap) }
            }
        }
    }

    /// Desmonta o que a casca pode desmontar. **Nunca** `quall_cleanup()` (dívida 2) — mesma
    /// regra do iOS e do Android, e pelo mesmo motivo: a função trava a libdatachannel num mutex
    /// global, e não há instante seguro para chamá-la aqui também (um processo de linha de
    /// comando de bancada não é diferente de um app nesse ponto).
    public func encerrar() {
        travaDoUso.lock()
        let quais = tracks, s = sessao
        tracks = []
        indiceDeVideo = nil
        indiceDeAudio = nil
        sessao = nil
        travaDoUso.unlock()
        // **Todas** as tracks antes da sessão, e não só a primeira: o header manda liberar as
        // tracks antes de `quall_session_close`, e com duas na mão esquecer a segunda vazaria um
        // handle por transmissão.
        for t in quais { quall_track_free(t) }
        if let s { quall_session_close(s) }
    }
}
