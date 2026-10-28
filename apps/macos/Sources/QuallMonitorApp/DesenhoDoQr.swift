import CoreImage
import CoreImage.CIFilterBuiltins
import Foundation

/// **O QR do convite** (`docs/conexao.md` §10.2), desenhado pelo `CIQRCodeGenerator` do sistema.
///
/// Nível de correção **M**, como o §10.2 pede: ~150 caracteres cabem na versão 7–8, legível a um braço
/// de distância. Um módulo vira um pixel; a tela amplia sem interpolar (`.interpolation(.none)`), e a
/// borda branca (a *quiet zone*) é do cartão, não da imagem.
///
/// O texto leva o segredo do convite: entra aqui e sai como pixels, nunca em diário.
enum DesenhoDoQr {
    private static let contexto = CIContext(options: [.useSoftwareRenderer: false])

    static func imagem(de texto: String) -> CGImage? {
        let filtro = CIFilter.qrCodeGenerator()
        filtro.message = Data(texto.utf8)
        filtro.correctionLevel = "M"
        guard let saida = filtro.outputImage else { return nil }
        return contexto.createCGImage(saida, from: saida.extent)
    }
}
