import AppKit
import CQuall
import SwiftUI
import QuallCaptureKit
import QuallIdiomaKit
import QuallMonitorKit
import QuallNetKit
import QuallReceptorKit

final class Modelo: ObservableObject {
    enum Papel: String, CaseIterable { case estender = "Estender tela", exibir = "Exibir" }
    enum Estado { case pronto, esperando, conectado, encerrando }
    @Published var papel: Papel = .estender
    @Published private(set) var estado: Estado = .pronto
    @Published var endereco = UserDefaults.standard.string(forKey: "monitor.endereco") ?? ""
    @Published var pin = ""
    @Published var escala = UserDefaults.standard.string(forKey: "monitor.escala") ?? "2x"
    @Published var fps = UserDefaults.standard.integer(forKey: "monitor.fps") == 60 ? 60 : 30
    @Published private(set) var mensagem = MensagemDoMonitor()
    @Published private(set) var pinDaEspera = ""
    @Published private(set) var enderecos: [String] = []
    @Published private(set) var nomeNaEspera = ""
    @Published private(set) var aparelhos: [Aparelho] = []
    @Published private(set) var monitores: [ControleDeMonitores.Monitor] = []
    @Published private(set) var monitoresRetidos = 0
    @Published private(set) var podeEsperarMais = false
    @Published private(set) var acessoRede: EstadoDaRedeLocal = .ocioso
    @Published private(set) var precisaPermissaoTela = false
    let exibidor = Exibidor()
    private var receptor: SessaoReceptora?
    private var sessoes: [Int: SessaoEmissora] = [:]
    private var controle = ControleDeMonitores(indices: UserDefaults.standard.data(forKey: "monitor.indices")
        .flatMap { try? JSONDecoder().decode(TabelaDeIndices.self, from: $0) } ?? .init())
    private var proximoId = 1
    private var encerrandoTudo = false
    private var configuracao: (escala: ModoDoMonitorVirtual.Escala, fps: Int)?
    private var descoberta: Descoberta?
    private let acessoARede = AcessoARedeLocal()
    private var iniciarAposRede: Papel?
    private var descobrirAposRede = false
    var ocupado: Bool { estado != .pronto }

    init() {
        quall_set_video_pacing_kbps(60_000)
        Registro.linha("Quall Monitor iniciou bundle=\(Bundle.main.bundleIdentifier ?? "sem bundle")")
        acessoARede.aoAtualizar = { [weak self] in self?.redeAtualizou($0) }
    }
    func descobrir() {
        guard acessoRede == .navegando else {
            descobrirAposRede = true
            acessoARede.iniciar()
            return
        }
        guard descoberta == nil else { return }
        let browser = Descoberta()
        browser.aoAtualizar = { [weak self] in self?.aparelhos = $0 }
        descoberta = browser
        browser.comecar()
    }
    func selecionar(_ aparelho: Aparelho) { endereco = aparelho.endereco; pin = "" }
    func iniciar() {
        guard !ocupado else { return }
        guard acessoRede == .navegando else {
            iniciarAposRede = papel
            acessoARede.iniciar(repetir: acessoRede == .bloqueado || acessoRede == .falha)
            return
        }
        mensagem = .init()
        precisaPermissaoTela = false
        switch papel {
        case .estender:
            guard MonitorVirtualAuxiliar.disponivel else {
                mensagem = .init("O monitor virtual não está disponível neste macOS. Confira se o app foi instalado completo.")
                return
            }
            guard PermissaoDeTela.estado() == .concedida || PermissaoDeTela.pedir() else {
                precisaPermissaoTela = true
                mensagem = .init("Permita o Quall Monitor em Gravação do Áudio do Sistema e da Tela. Se ele não aparecer, use + para adicionar Quall Monitor.app em Aplicativos. Depois, feche e abra o app.")
                return
            }
            configuracao = (.init(rawValue: escala) ?? .dobro, fps)
            guard controle.novaRodada() else { return }
            encerrandoTudo = false
            UserDefaults.standard.set(escala, forKey: "monitor.escala")
            UserDefaults.standard.set(fps, forKey: "monitor.fps")
            abrirEspera()
        case .exibir:
            iniciarRecepcao()
        }
    }
    func mudouPapel() {
        iniciarAposRede = nil; descobrirAposRede = false
        if papel == .exibir { descobrir() }
    }
    func repetirAcessoRede() {
        if papel == .exibir { descobrirAposRede = true }
        acessoARede.iniciar(repetir: true)
    }
    private func redeAtualizou(_ next: EstadoDaRedeLocal) {
        acessoRede = next
        if next == .navegando {
            if descobrirAposRede { descobrirAposRede = false; descobrir() }
            let pending = iniciarAposRede
            iniciarAposRede = nil
            if pending == papel, !ocupado { iniciar() }
        } else if next == .bloqueado, ocupado {
            iniciarAposRede = nil
            parar()
        }
    }
    func abrirAjustesRede() {
        NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork")!)
    }
    func abrirAjustesTela() {
        NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")!)
    }
    /// One advertised wait, as in Studio. Each receiver owns a separate port/session/helper/encoder.
    func abrirEspera() {
        guard let configuration = configuracao, !encerrandoTudo, controle.podeAbrirEspera else { return }
        let port = Rede.portaLivre()
        guard port > 0 else { mensagem = .init("Não consegui abrir uma porta local."); publicar(); return }
        let generated = NucleoDeRede.sortearPin()
        guard generated.count == 6 else { mensagem = .init("Não consegui gerar o PIN. Tente novamente."); publicar(); return }
        let id = proximoId; proximoId += 1
        guard controle.abrirEspera(id: id) else { return }
        let session = SessaoEmissora(id: id, porta: port, pin: generated,
                                     escala: configuration.escala, fps: configuration.fps)
        sessoes[id] = session
        nomeNaEspera = ""
        session.aoAnunciar = { [weak self] label in
            guard let self, self.controle.espera?.id == id else { return }
            self.nomeNaEspera = label
        }
        session.aoParear = { [weak self] peer in self?.pareou(id: id, par: peer) }
        session.aoEstado = { [weak self] text in
            guard let self, !self.encerrandoTudo else { return }
            self.controle.iniciou(id: id, mensagem: text)
            self.publicar()
        }
        session.aoTerminar = { [weak self] text, released in
            self?.terminouEmissao(id: id, motivo: text, monitorLiberado: released)
        }
        mensagem = .init("No outro aparelho, abra Exibir e conecte pelo nome ou endereço abaixo.")
        publicar()
        session.comecar()
    }
    private func pareou(id: Int, par: ParDoMonitor) {
        guard !encerrandoTudo, let old = controle.pareou(id: id, par: par) else {
            sessoes[id]?.parar(); return
        }
        for id in old { sessoes[id]?.parar() }
        prepararPendentes()
        publicar()
    }
    private func prepararPendentes() {
        guard !encerrandoTudo else { return }
        var started = false
        for monitor in controle.conectados where monitor.fase == .preparando {
            guard let index = controle.preparar(id: monitor.id, agora: Date().timeIntervalSince1970) else { continue }
            sessoes[monitor.id]?.preparar(indice: index)
            started = true
        }
        if started {
            if let data = try? JSONEncoder().encode(controle.indices) {
                UserDefaults.standard.set(data, forKey: "monitor.indices")
            }
            abrirEspera()
        }
    }
    func desconectar(_ id: Int) {
        controle.encerrar(id: id)
        sessoes[id]?.parar()
        publicar()
    }
    private func terminouEmissao(id: Int, motivo: MensagemDoMonitor, monitorLiberado: Bool) {
        let wasWaiting = controle.monitores[id]?.fase == .esperando
        controle.remover(id: id, monitorLiberado: monitorLiberado)
        sessoes[id] = nil
        if !motivo.chave.isEmpty { mensagem = motivo }
        if encerrandoTudo || sessoes.isEmpty {
            if sessoes.isEmpty { configuracao = nil; encerrandoTudo = false }
        } else {
            prepararPendentes()
            // Failed/expired waits need an explicit retry; never spin on failed handshakes.
            if !wasWaiting { abrirEspera() }
        }
        publicar()
    }
    private func publicar() {
        monitores = controle.conectados
        monitoresRetidos = controle.indicesEmQuarentena.count
        if let waiting = controle.espera, let session = sessoes[waiting.id] {
            pinDaEspera = session.pin
            enderecos = Rede.enderecos(porta: session.porta)
        } else { pinDaEspera = ""; enderecos = []; nomeNaEspera = "" }
        podeEsperarMais = configuracao != nil && !encerrandoTudo && controle.podeAbrirEspera
        if encerrandoTudo { estado = .encerrando }
        else if sessoes.isEmpty { estado = .pronto }
        else if monitores.contains(where: { $0.fase == .ativo }) { estado = .conectado }
        else { estado = .esperando }
    }
    private func iniciarRecepcao() {
        let target = endereco.trimmingCharacters(in: .whitespacesAndNewlines)
        let digits = pin.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !target.isEmpty else { mensagem = .init("Digite o endereço e a porta mostrados no emissor."); return }
        guard digits.isEmpty || (digits.count == 6 && digits.allSatisfy { $0 >= "0" && $0 <= "9" }) else {
            mensagem = .init("O PIN deve ter seis dígitos. Deixe vazio para retomar um pareamento salvo."); return
        }
        let parsed = target.withCString { source in
            lerTexto { out, count in Int(quall_parse_endpoint_json(source, nil, out, count)) }
        }
        guard !parsed.isEmpty else { mensagem = .init("Endereço inválido. Use IP:porta, nome:porta ou [IPv6]:porta."); return }
        let hasPort = target.hasPrefix("[") ? target.contains("]:") : target.filter { $0 == ":" }.count == 1
        guard hasPort else { mensagem = .init("Inclua a porta indicada no emissor: por exemplo, 192.168.1.20:7878."); return }
        UserDefaults.standard.set(target, forKey: "monitor.endereco")
        exibidor.reiniciar()
        exibidor.camada.flushAndRemoveImage()
        let screen = NSApp.keyWindow?.screen ?? NSScreen.main
        let pixels = screen.map { (UInt32(($0.frame.width * $0.backingScaleFactor).rounded()),
                                   UInt32(($0.frame.height * $0.backingScaleFactor).rounded())) } ?? (1920, 1080)
        let session = SessaoReceptora(endereco: target, pin: digits, tela: pixels, exibidor: exibidor)
        receptor = session
        estado = .esperando
        mensagem = .init("Conectando…")
        session.aoEstado = { [weak self] text in
            guard let self, self.estado != .encerrando else { return }
            self.estado = .conectado; self.mensagem = text
        }
        session.aoTerminar = { [weak self] text in
            self?.receptor = nil; self?.estado = .pronto; self?.mensagem = text; self?.pin = ""
        }
        session.comecar()
    }
    func parar() {
        guard ocupado else { return }
        mensagem = .init("Encerrando a sessão…")
        estado = .encerrando
        if let receptor { receptor.parar(); return }
        encerrandoTudo = true
        controle.encerrarTodos()
        for session in sessoes.values { session.parar() }
        publicar()
    }
    func sair() {
        iniciarAposRede = nil; descobrirAposRede = false
        acessoARede.parar(); descoberta?.parar(); parar()
    }
}

@main
struct QuallMonitorApp: App {
    @NSApplicationDelegateAdaptor(Delegado.self) private var delegado
    @StateObject private var modelo = Modelo()
    @ObservedObject private var idioma = TrocaDeIdioma.compartilhada
    init() { Verificacao.executarSePedida() }
    var body: some Scene {
        Window("Quall Monitor", id: "monitor.principal") {
            TelaPrincipal(modelo: modelo)
                .frame(minWidth: 680, minHeight: 520)
                .preferredColorScheme(.dark)
                .onAppear { delegado.modelo = modelo }
        }
        .defaultSize(width: 850, height: 680)
        .commands {
            CommandGroup(replacing: .newItem) { }
            CommandGroup(after: .windowArrangement) {
                Button(T("Tela cheia")) { NSApp.keyWindow?.toggleFullScreen(nil) }
                    .keyboardShortcut("f", modifiers: [.command, .control])
            }
        }
    }
}

private struct TelaPrincipal: View {
    @ObservedObject var modelo: Modelo
    @ObservedObject private var idioma = TrocaDeIdioma.compartilhada
    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            cabecalho
            permissoes
            if modelo.papel == .exibir && modelo.ocupado {
                VistaDeVideo(camada: modelo.exibidor.camada)
                    .frame(maxWidth: .infinity, maxHeight: .infinity).background(.black)
                    .clipShape(RoundedRectangle(cornerRadius: 12))
                HStack {
                    Text(modelo.mensagem.texto).font(.callout).foregroundStyle(.secondary)
                    Spacer()
                    Button(T("Tela cheia")) { NSApp.keyWindow?.toggleFullScreen(nil) }
                }
            } else {
                Picker(T("Papel deste Mac"), selection: $modelo.papel) {
                    ForEach(Modelo.Papel.allCases, id: \.self) { Text(T($0.rawValue)).tag($0) }
                }.pickerStyle(.segmented).disabled(modelo.ocupado)
                ScrollView {
                    VStack(alignment: .leading, spacing: 20) {
                        if modelo.papel == .estender { estender } else { exibir }
                        if !modelo.mensagem.chave.isEmpty {
                            Text(modelo.mensagem.texto).font(.callout).foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                }
                Text(Identidade.nome).font(.caption).foregroundStyle(.secondary)
            }
        }.padding(28).background(Color(red: 0.055, green: 0.065, blue: 0.08))
        .onChange(of: modelo.papel) { _ in modelo.mudouPapel() }
    }
    @ViewBuilder private var permissoes: some View {
        if modelo.acessoRede == .bloqueado || modelo.acessoRede == .falha {
            VStack(alignment: .leading, spacing: 8) {
                Text(T(modelo.acessoRede == .bloqueado
                    ? "O acesso à rede local está bloqueado. Permita o Quall Monitor em Ajustes do Sistema › Privacidade e Segurança › Rede Local."
                    : "Não consegui iniciar a descoberta na rede local. Confira a conexão e tente novamente."))
                    .font(.callout).foregroundStyle(.orange)
                HStack {
                    Button(T("Abrir Ajustes de Rede Local")) { modelo.abrirAjustesRede() }
                    Button(T("Verificar novamente")) { modelo.repetirAcessoRede() }
                }
            }
        } else if modelo.acessoRede == .aguardando {
            Text(T("Aguardando acesso à rede local. Responda ao pedido do macOS, se aparecer."))
                .font(.callout).foregroundStyle(.secondary)
        }
        if modelo.precisaPermissaoTela && modelo.papel == .estender {
            Button(T("Abrir Ajustes de Gravação da Tela")) { modelo.abrirAjustesTela() }
        }
    }
    private var cabecalho: some View {
        HStack {
            Image(nsImage: NSImage(named: NSImage.applicationIconName) ?? NSImage())
                .resizable().frame(width: 42, height: 42)
            VStack(alignment: .leading, spacing: 4) {
                Text("Quall Monitor").font(.title2.bold())
                Text(T("Até 8 monitores na sua rede local.")).foregroundStyle(.secondary)
            }
            Spacer()
            Picker(T("Idioma"), selection: Binding(get: { idioma.atual }, set: { idioma.escolher($0) })) {
                Text("PT").tag(Idioma.pt); Text("EN").tag(Idioma.en)
            }.pickerStyle(.segmented).frame(width: 94).accessibilityLabel(T("Idioma"))
            if modelo.ocupado {
                Button(T(modelo.papel == .estender ? "Parar todos" : "Parar")) { modelo.parar() }
                    .disabled(modelo.estado == .encerrando)
            }
        }
    }
    private var estender: some View {
        VStack(alignment: .leading, spacing: 20) {
            if modelo.monitoresRetidos > 0 {
                Text(T("Não foi possível confirmar a saída de %@ monitor(es). Suas vagas permanecem reservadas nesta execução do app.", String(modelo.monitoresRetidos)))
                    .font(.callout).foregroundStyle(.orange)
            }
            if modelo.ocupado {
                if !modelo.monitores.isEmpty {
                    Text(T("Monitores: %@ de 8", String(modelo.monitores.count))).font(.title3.bold())
                    ForEach(modelo.monitores) { monitor in linha(monitor) }
                    Button(T("Organizar monitores no macOS")) {
                        NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.Displays-Settings.extension")!)
                    }
                }
                if !modelo.pinDaEspera.isEmpty {
                    Text(T(modelo.monitores.isEmpty ? "Conecte o outro aparelho" : "Conecte mais um aparelho"))
                        .font(.title3.bold())
                    if !modelo.nomeNaEspera.isEmpty {
                        Text(T("Nome na rede: %@", modelo.nomeNaEspera)).textSelection(.enabled)
                    }
                    Text(modelo.pinDaEspera).font(.system(size: 44, weight: .semibold, design: .monospaced))
                        .textSelection(.enabled).accessibilityLabel(T("PIN %@", modelo.pinDaEspera))
                    ForEach(modelo.enderecos, id: \.self) { address in
                        HStack {
                            Text(address).font(.system(.body, design: .monospaced)).textSelection(.enabled)
                            Spacer()
                            Button(T("Copiar")) { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(address, forType: .string) }
                        }
                    }
                    Text(T("Use sempre o PIN e o endereço atuais para conectar o próximo aparelho."))
                        .font(.caption).foregroundStyle(.secondary)
                } else if modelo.podeEsperarMais {
                    Button(T("Conectar mais um aparelho")) { modelo.abrirEspera() }
                } else if modelo.monitores.count == ControleDeMonitores.limite {
                    Text(T("Limite de 8 monitores. Desconecte um para liberar uma vaga.")).foregroundStyle(.secondary)
                }
            } else {
                Text(T("Estenda a área de trabalho deste Mac")).font(.title3.bold())
                Text(T("Cada aparelho recebe uma tela própria, até 8 ao mesmo tempo. O monitor é criado quando ele conecta e desaparece ao parar."))
                    .foregroundStyle(.secondary)
                HStack(spacing: 20) {
                    Picker(T("Escala"), selection: $modelo.escala) {
                        Text(T("2x · texto nítido")).tag("2x"); Text(T("1x · mais espaço")).tag("1x")
                    }
                    Picker(T("Quadros"), selection: $modelo.fps) { Text("30 fps").tag(30); Text("60 fps").tag(60) }
                }
                Button(T("Iniciar tela estendida")) { modelo.iniciar() }.buttonStyle(.borderedProminent).tint(.cyan)
                    .disabled(modelo.monitoresRetidos == ControleDeMonitores.limite)
            }
        }
    }
    private func linha(_ monitor: ControleDeMonitores.Monitor) -> some View {
        HStack {
            VStack(alignment: .leading, spacing: 4) {
                Text(monitor.nome).font(.headline)
                Text(monitor.fase == .ativo ? monitor.mensagem.texto : T(monitor.fase == .encerrando ? "Encerrando…" : "Preparando o monitor…"))
                    .font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            Button(T("Desconectar")) { modelo.desconectar(monitor.id) }.disabled(monitor.fase == .encerrando)
        }.padding(12).background(Color.white.opacity(0.045)).clipShape(RoundedRectangle(cornerRadius: 10))
    }
    private var exibir: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(T("Use este Mac como monitor")).font(.title3.bold())
            Text(T("No outro computador, inicie Estender tela. Use o endereço completo e o PIN exibidos lá."))
                .foregroundStyle(.secondary)
            TextField(T("Endereço do emissor · IP:porta"), text: $modelo.endereco).textFieldStyle(.roundedBorder)
            TextField(T("PIN · 6 dígitos (vazio para pareamento salvo)"), text: $modelo.pin).textFieldStyle(.roundedBorder)
                .onSubmit { modelo.iniciar() }
            Button(T("Conectar")) { modelo.iniciar() }.buttonStyle(.borderedProminent).tint(.cyan)
            if !modelo.aparelhos.isEmpty {
                Text(T("Aparelhos na rede")).font(.subheadline.bold())
                ForEach(modelo.aparelhos) { device in
                    Button { modelo.selecionar(device) } label: {
                        HStack { Text(device.nome); Spacer(); Text(device.endereco).foregroundStyle(.secondary) }
                    }.buttonStyle(.plain)
                }
            }
        }
    }
}

final class Delegado: NSObject, NSApplicationDelegate {
    weak var modelo: Modelo?
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let modelo, modelo.ocupado else { modelo?.sair(); return .terminateNow }
        modelo.sair()
        func poll() {
            if !modelo.ocupado { sender.reply(toApplicationShouldTerminate: true); return }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1, execute: poll)
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1, execute: poll)
        return .terminateLater
    }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}
