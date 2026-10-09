import AppKit
import CQuall
import SwiftUI
import QuallCaptureKit
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
    @Published private(set) var mensagem = ""
    @Published private(set) var pinDaEspera = ""
    @Published private(set) var enderecos: [String] = []
    @Published private(set) var aparelhos: [Aparelho] = []
    let exibidor = Exibidor()
    private var sessao: Sessao?
    private var descoberta: Descoberta?
    var ocupado: Bool { estado != .pronto }

    init() {
        quall_set_video_pacing_kbps(60_000)
        Registro.linha("Quall Monitor iniciou bundle=\(Bundle.main.bundleIdentifier ?? "sem bundle")")
    }
    func descobrir() {
        guard descoberta == nil else { return }
        let browser = Descoberta()
        browser.aoAtualizar = { [weak self] in self?.aparelhos = $0 }
        descoberta = browser
        browser.comecar()
    }
    func selecionar(_ aparelho: Aparelho) { endereco = aparelho.endereco; pin = "" }
    func iniciar() {
        guard !ocupado else { return }
        mensagem = ""
        switch papel {
        case .estender:
            guard MonitorVirtualAuxiliar.disponivel else {
                mensagem = "O monitor virtual não está disponível neste macOS. Confira se o app foi instalado completo."
                return
            }
            guard PermissaoDeTela.estado() == .concedida || PermissaoDeTela.pedir() else {
                mensagem = "Permita o Quall Monitor em Ajustes do Sistema › Privacidade e Segurança › Gravação da Tela e abra o app novamente."
                return
            }
            let port = Rede.portaLivre()
            guard port > 0 else { mensagem = "Não consegui abrir uma porta local."; return }
            let generated = NucleoDeRede.sortearPin()
            guard generated.count == 6 else { mensagem = "Não consegui gerar o PIN. Tente novamente."; return }
            let session = SessaoEmissora(porta: port, pin: generated,
                                        escala: .init(rawValue: escala) ?? .dobro, fps: fps)
            sessao = session
            pinDaEspera = generated
            enderecos = Rede.enderecos(porta: port)
            UserDefaults.standard.set(escala, forKey: "monitor.escala")
            UserDefaults.standard.set(fps, forKey: "monitor.fps")
            estado = .esperando
            mensagem = "No outro aparelho, abra Exibir e conecte pelo nome ou endereço abaixo."
            session.aoEstado = { [weak self] text in
                guard let self, self.estado != .encerrando else { return }
                self.estado = .conectado; self.mensagem = text
            }
            session.aoTerminar = { [weak self] text in self?.terminou(text) }
            session.comecar()
        case .exibir:
            let target = endereco.trimmingCharacters(in: .whitespacesAndNewlines)
            let digits = pin.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !target.isEmpty else { mensagem = "Digite o endereço e a porta mostrados no emissor."; return }
            guard digits.isEmpty || (digits.count == 6 && digits.allSatisfy { $0 >= "0" && $0 <= "9" }) else {
                mensagem = "O PIN deve ter seis dígitos. Deixe vazio para retomar um pareamento salvo."
                return
            }
            // The explicit port avoids confusing Monitor 7878 with Studio's default 7877.
            let parsed = target.withCString { source in
                lerTexto { out, count in Int(quall_parse_endpoint_json(source, nil, out, count)) }
            }
            guard !parsed.isEmpty else { mensagem = "Endereço inválido. Use IP:porta, nome:porta ou [IPv6]:porta."; return }
            // Supplying a bare host is ambiguous; discovery always supplies an explicit port.
            let hasPort = target.hasPrefix("[") ? target.contains("]:") : target.filter { $0 == ":" }.count == 1
            guard hasPort else { mensagem = "Inclua a porta indicada no emissor: por exemplo, 192.168.1.20:7878."; return }
            UserDefaults.standard.set(target, forKey: "monitor.endereco")
            exibidor.reiniciar()
            exibidor.camada.flushAndRemoveImage()
            let screen = NSApp.keyWindow?.screen ?? NSScreen.main
            let pixels = screen.map { (UInt32(($0.frame.width * $0.backingScaleFactor).rounded()),
                                       UInt32(($0.frame.height * $0.backingScaleFactor).rounded())) } ?? (1920, 1080)
            let session = SessaoReceptora(endereco: target, pin: digits, tela: pixels, exibidor: exibidor)
            sessao = session
            estado = .esperando
            mensagem = "Conectando…"
            session.aoEstado = { [weak self] text in
                guard let self, self.estado != .encerrando else { return }
                self.estado = .conectado; self.mensagem = text
            }
            session.aoTerminar = { [weak self] text in self?.terminou(text) }
            session.comecar()
        }
    }
    func parar() {
        guard sessao != nil else { return }
        estado = .encerrando
        mensagem = "Encerrando a sessão…"
        sessao?.parar()
    }
    private func terminou(_ text: String) {
        sessao = nil
        estado = .pronto
        pinDaEspera = ""
        mensagem = text
        pin = ""
    }
    func sair() { descoberta?.parar(); parar() }
}

@main
struct QuallMonitorApp: App {
    @NSApplicationDelegateAdaptor(Delegado.self) private var delegado
    @StateObject private var modelo = Modelo()
    init() { Verificacao.executarSePedida() }
    var body: some Scene {
        Window("Quall Monitor", id: "monitor.principal") {
            TelaPrincipal(modelo: modelo)
                .frame(minWidth: 680, minHeight: 520)
                .preferredColorScheme(.dark)
                .onAppear { delegado.modelo = modelo }
        }
        .defaultSize(width: 800, height: 600)
        .commands {
            CommandGroup(replacing: .newItem) { }
            CommandGroup(after: .windowArrangement) {
                Button("Tela cheia") { NSApp.keyWindow?.toggleFullScreen(nil) }
                    .keyboardShortcut("f", modifiers: [.command, .control])
            }
        }
    }
}

private struct TelaPrincipal: View {
    @ObservedObject var modelo: Modelo
    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            HStack {
                Image(systemName: "display.2").font(.system(size: 28)).foregroundStyle(.cyan)
                VStack(alignment: .leading, spacing: 4) {
                    Text("Quall Monitor").font(.title2.bold())
                    Text("Um monitor a mais na sua rede local.").foregroundStyle(.secondary)
                }
                Spacer()
                if modelo.ocupado {
                    Button("Parar") { modelo.parar() }.disabled(modelo.estado == .encerrando)
                }
            }
            if modelo.papel == .exibir && modelo.ocupado {
                VistaDeVideo(camada: modelo.exibidor.camada)
                    .frame(maxWidth: .infinity, maxHeight: .infinity).background(.black)
                    .clipShape(RoundedRectangle(cornerRadius: 12))
                HStack {
                    Text(modelo.mensagem).font(.callout).foregroundStyle(.secondary)
                    Spacer()
                    Button("Tela cheia") { NSApp.keyWindow?.toggleFullScreen(nil) }
                }
            } else {
                Picker("Papel deste Mac", selection: $modelo.papel) {
                    ForEach(Modelo.Papel.allCases, id: \.self) { Text($0.rawValue).tag($0) }
                }
                .pickerStyle(.segmented).disabled(modelo.ocupado)
                if modelo.papel == .estender { estender } else { exibir }
                if !modelo.mensagem.isEmpty {
                    Text(modelo.mensagem).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 0)
                Text(Identidade.nome).font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(28)
        .background(Color(red: 0.055, green: 0.065, blue: 0.08))
        .onChange(of: modelo.papel) { value in if value == .exibir { modelo.descobrir() } }
    }
    private var estender: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text(modelo.ocupado ? "Conecte o outro aparelho" : "Estenda a área de trabalho deste Mac")
                .font(.title3.bold())
            if modelo.ocupado {
                Text(modelo.pinDaEspera).font(.system(size: 48, weight: .semibold, design: .monospaced))
                    .textSelection(.enabled).accessibilityLabel("PIN \(modelo.pinDaEspera)")
                ForEach(modelo.enderecos, id: \.self) { address in
                    HStack {
                        Text(address).font(.system(.body, design: .monospaced)).textSelection(.enabled)
                        Spacer()
                        Button("Copiar") { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(address, forType: .string) }
                    }
                }
                if modelo.enderecos.isEmpty { Text("Conecte os dois aparelhos à mesma rede local.").foregroundStyle(.orange) }
                if modelo.estado == .conectado {
                    Button("Organizar monitores no macOS") {
                        NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.Displays-Settings.extension")!)
                    }
                }
            } else {
                Text("O outro aparelho recebe uma tela própria. O monitor é criado quando ele conecta e desaparece ao parar.")
                    .foregroundStyle(.secondary)
                HStack(spacing: 20) {
                    Picker("Escala", selection: $modelo.escala) {
                        Text("2x · texto nítido").tag("2x")
                        Text("1x · mais espaço").tag("1x")
                    }
                    Picker("Quadros", selection: $modelo.fps) { Text("30 fps").tag(30); Text("60 fps").tag(60) }
                }
                Button("Iniciar tela estendida") { modelo.iniciar() }
                    .buttonStyle(.borderedProminent).tint(.cyan)
            }
        }
    }
    private var exibir: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Use este Mac como monitor").font(.title3.bold())
            Text("No outro computador, inicie Estender tela. Use o endereço completo e o PIN exibidos lá.")
                .foregroundStyle(.secondary)
            TextField("Endereço do emissor · IP:porta", text: $modelo.endereco).textFieldStyle(.roundedBorder)
            TextField("PIN · 6 dígitos (vazio para pareamento salvo)", text: $modelo.pin).textFieldStyle(.roundedBorder)
                .onSubmit { modelo.iniciar() }
            Button("Conectar") { modelo.iniciar() }.buttonStyle(.borderedProminent).tint(.cyan)
            if !modelo.aparelhos.isEmpty {
                Text("Aparelhos na rede").font(.subheadline.bold())
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
        // Wait for ordered teardown: capture, helper, tracks, then the network session.
        func poll() {
            if !modelo.ocupado { sender.reply(toApplicationShouldTerminate: true); return }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1, execute: poll)
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1, execute: poll)
        return .terminateLater
    }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}
