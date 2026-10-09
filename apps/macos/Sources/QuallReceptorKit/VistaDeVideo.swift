import AVFoundation
import AppKit
import SwiftUI

/// A camada de exibição, embrulhada para o SwiftUI do macOS.
///
/// # A armadilha que custou uma investigação inteira no iOS, e a porta a mais que o AppKit tem
///
/// No receptor do iPad, `makeUIView` devolvia uma `UIView()` crua e pendurava a camada como
/// sublayer à mão, com o quadro acertado só no `updateUIView` — que o SwiftUI chama quando o
/// **estado** muda, não a cada passada de layout. Enquanto a vista não tinha sido medida, `bounds`
/// era `.zero`, e a camada ficava com **quadro zero**. Uma `AVSampleBufferDisplayLayer` de quadro
/// zero **aceita quadros para sempre e não desenha nada**: `enqueue` devolve normalmente,
/// `isReadyForMoreMediaData` continua `true`, `status` nunca vira `.failed`. O sintoma medido foi
/// `recebidos 883 · exibidos 883 · decode p50 6,50 ms` com **a tela preta**.
///
/// O AppKit tem a mesma armadilha e **mais uma, anterior a ela**: uma `NSView` não tem `layer`
/// nenhuma até `wantsLayer` ser `true`. `layer?.addSublayer(camada)` com `wantsLayer` desligado é
/// um `nil`-coalescing silencioso — a camada nunca entra na árvore, e o sintoma é idêntico ao do
/// quadro zero: todos os contadores fecham e a janela fica preta. Por isso `wantsLayer` é a
/// **primeira** linha do `init`, antes de qualquer coisa que toque em `layer`, e por isso
/// `Exibidor.Instantaneo` publica `camadaNaArvore` ao lado de `largura x altura`: as duas portas
/// levam ao mesmo lugar e o relato precisa distinguir qual delas foi.
///
/// # Por que a camada não é a `backingLayer` desta vista
///
/// Porque o `Exibidor` é dono dela e vive mais que esta vista: a corrida pode começar antes de a
/// tela montar, e a vista sai da árvore a cada volta ao formulário. `makeBackingLayer()` — o
/// `layerClass` do AppKit — exige que a camada nasça e morra com a vista. O preço da escolha é
/// acompanhar o quadro à mão, e o lugar de fazer isso é `layout()`, que roda em **toda** passada
/// de layout: a primeira medição real, todo redimensionamento da janela, toda troca de tela.
public struct VistaDeVideo: NSViewRepresentable {

    /// A vista que **de fato** carrega a camada, e que acompanha o próprio tamanho.
    public final class Vista: NSView {
        let camada: AVSampleBufferDisplayLayer

        init(camada: AVSampleBufferDisplayLayer) {
            self.camada = camada
            super.init(frame: .zero)
            // **Primeiro isto.** Sem `wantsLayer`, `self.layer` é `nil` e a linha de baixo não faz
            // nada — em silêncio. Ver o cabeçalho do tipo.
            wantsLayer = true
            layer?.backgroundColor = CGColor(red: 0, green: 0, blue: 0, alpha: 1)
            layer?.masksToBounds = true
            camada.videoGravity = .resizeAspect
            layer?.addSublayer(camada)
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) { fatalError("só por código") }

        /// **Não** sobrescrevemos `isFlipped`. Um `NSView` marcado como invertido faz o AppKit
        /// ligar `isGeometryFlipped` na camada de fundo, e aí o vídeo sai de cabeça para baixo —
        /// um defeito que nenhum contador acusa e que só aparece com alguém olhando.
        public override func layout() {
            super.layout()
            guard camada.frame != bounds else { return }
            // Sem desligar as ações, todo redimensionamento da janela anima a camada e o vídeo
            // "escorrega" atrás da borda por um quarto de segundo.
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            camada.frame = bounds
            CATransaction.commit()
        }

        /// A escala muda quando a janela passa de um monitor Retina para um comum, e uma camada
        /// com a escala do monitor anterior desenha borrada ou serrilhada.
        public override func viewDidChangeBackingProperties() {
            super.viewDidChangeBackingProperties()
            let escala = window?.backingScaleFactor ?? 2
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            layer?.contentsScale = escala
            camada.contentsScale = escala
            CATransaction.commit()
        }
    }

    let camada: AVSampleBufferDisplayLayer

    public init(camada: AVSampleBufferDisplayLayer) {
        self.camada = camada
    }

    public func makeNSView(context: Context) -> Vista { Vista(camada: camada) }

    public func updateNSView(_ v: Vista, context: Context) {
        // De propósito quase vazio: quem acerta o quadro é o `layout()` da `Vista`. Pedir uma
        // passada aqui cobre o caso de o SwiftUI trocar o tamanho sem invalidar o layout.
        v.needsLayout = true
    }

    /// Desmonta a camada quando a vista sai da árvore.
    ///
    /// **A camada é do `Exibidor` e sobrevive a esta vista** — é o que permite a corrida começar
    /// antes de a tela montar. O preço é que largar a vista **não** larga a camada: ela fica
    /// pendurada na `layer` da vista antiga, guardando o **último quadro decodificado**, e volta a
    /// aparecer por cima do que vier depois. Num receptor que exibe a tela de outra pessoa isso
    /// não é feiura, é **vazamento de conteúdo entre sessões**.
    public static func dismantleNSView(_ v: Vista, coordinator: ()) {
        v.camada.flushAndRemoveImage()
        v.camada.removeFromSuperlayer()
    }
}
