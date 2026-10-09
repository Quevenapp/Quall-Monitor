//! **De onde a janela tira a imagem**, quadro a quadro: a aritmética do retângulo de origem do
//! Video Processor da exibição (`present.rs`), sem Win32, para ser testada em qualquer máquina.
//!
//! # O defeito que isto conserta (o G1 da crítica de 21/09, medido no controle da troca)
//!
//! O `Presenter` nascia com a largura e a altura do primeiro SPS e usava esse retângulo como
//! origem em **todo** quadro. Quando o fluxo trocava de tamanho no meio da sessão, o decoder
//! renegociava (`MF_E_TRANSFORM_STREAM_CHANGE`) e a exibição relia a abertura, mas a origem
//! continuava a do primeiro SPS. O controle de 21/09 (`.h264` sintético 854×480 → 640×480 → 854×480
//! mandado ao app de hoje, no Dell) mostrou os dois sentidos:
//! - **descendo, de 854 para 640**: a origem de 854 passava da textura de 640 e o `present_frame`
//!   falhava com `0x80070057` (parâmetro incorreto) em todo quadro. A janela ficou parada na última
//!   imagem de 854 enquanto o fluxo esteve em 640, e voltou a andar quando ele voltou a 854;
//! - **subindo, de 640 para 854** (uma sessão que abre em 640): a origem de 640 cabia na textura
//!   de 864 e pegava só as 640 colunas da esquerda. Esse retângulo esticado no destino 16:9 dava uma
//!   imagem **cortada à direita e esticada**, sem erro nenhum. É um cenário lido no código, sem
//!   medida.
//!
//! O passo 11 do `anel` (21/09, no Dell) disse qual chamada recusava: o `VideoProcessorBlt`, com a
//! origem de 854 numa textura de 640 (`0x80070057`). A vista de entrada com o enumerador de 854
//! aceita a textura de 640.
//!
//! O conserto é pedir a origem **por quadro**: a abertura visível que o decoder declarou
//! (`MF_MT_MINIMUM_DISPLAY_APERTURE`), recortada ao tamanho da textura que chegou. Uma origem
//! nunca passa da textura, qualquer que seja a ordem em que o tipo e a textura mudarem.

/// Um retângulo em pixels, `(esquerda, topo, direita, base)`, como o `RECT` do Win32.
pub type Retangulo = (i32, i32, i32, i32);

/// **A origem da apresentação**: a abertura visível, recortada à textura. `None` quando não sobra
/// área (uma abertura fora da textura, ou uma textura vazia): o quadro não é apresentado, e o
/// chamador conta e segue.
pub fn origem_na_textura(visivel: Retangulo, textura: (u32, u32)) -> Option<Retangulo> {
    let (tl, ta) = (textura.0.min(i32::MAX as u32) as i32, textura.1.min(i32::MAX as u32) as i32);
    let esquerda = visivel.0.clamp(0, tl);
    let topo = visivel.1.clamp(0, ta);
    let direita = visivel.2.clamp(0, tl);
    let base = visivel.3.clamp(0, ta);
    (direita > esquerda && base > topo).then_some((esquerda, topo, direita, base))
}

/// O tamanho de um retângulo.
pub fn tamanho(r: Retangulo) -> (u32, u32) {
    ((r.2 - r.0).max(0) as u32, (r.3 - r.1).max(0) as u32)
}

#[cfg(test)]
mod testes {
    use super::*;

    /// **O controle de 21/09, descendo**: a sessão abriu em 854×480 (textura 864×480, "só o
    /// alinhamento") e o fluxo passou a 640×480. A origem antiga, fixa no primeiro SPS, era
    /// (0, 0, 854, 480): maior que a textura de 640, e o `present_frame` devolvia `0x80070057` em
    /// todo quadro. A origem nova é a abertura de agora, dentro da textura.
    #[test]
    fn g1_descendo_de_854_para_640_a_origem_cabe_na_textura() {
        let origem_antiga = (0, 0, 854, 480);
        assert!(origem_na_textura(origem_antiga, (640, 480)) != Some(origem_antiga), "a antiga passava da textura");
        assert_eq!(origem_na_textura((0, 0, 640, 480), (640, 480)), Some((0, 0, 640, 480)));
        // Com o tipo relido atrasado um quadro (a abertura ainda a de 854), a origem recorta à
        // textura em vez de passar dela.
        assert_eq!(origem_na_textura((0, 0, 854, 480), (640, 480)), Some((0, 0, 640, 480)));
    }

    /// **Subindo, de 640 para 854** (a sessão que abre em 4:3): a origem antiga, (0, 0, 640, 480),
    /// cabia na textura de 864 e cortava a imagem à direita. A nova pega a abertura inteira, 854,
    /// sem a coluna de enchimento do alinhamento.
    #[test]
    fn g1_subindo_de_640_para_854_a_origem_pega_a_imagem_inteira() {
        let origem = origem_na_textura((0, 0, 854, 480), (864, 480)).unwrap();
        assert_eq!(origem, (0, 0, 854, 480));
        assert_eq!(tamanho(origem), (854, 480), "nem as 640 da esquerda, nem as 864 da textura");
        // A abertura com deslocamento (um decodificador que recorta em cima) é respeitada.
        assert_eq!(origem_na_textura((0, 4, 1920, 1084), (1920, 1088)), Some((0, 4, 1920, 1084)));
    }

    #[test]
    fn a_origem_vazia_nao_apresenta() {
        assert_eq!(origem_na_textura((0, 0, 0, 480), (640, 480)), None);
        assert_eq!(origem_na_textura((700, 0, 854, 480), (640, 480)), None, "a abertura toda fora da textura");
        assert_eq!(origem_na_textura((0, 0, 640, 480), (0, 0)), None);
        assert_eq!(origem_na_textura((-5, -5, 10, 10), (640, 480)), Some((0, 0, 10, 10)));
    }
}
