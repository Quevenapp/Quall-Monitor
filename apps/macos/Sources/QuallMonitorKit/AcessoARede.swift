/// Interpretation of a real Bonjour operation, not a general permission preflight.
public enum EstadoDaRedeLocal: Equatable, Sendable {
    case ocioso, aguardando, navegando, bloqueado, falha
    public static func deBonjour(pronto: Bool = false, codigoDNS: Int32? = nil, falhou: Bool = false) -> Self {
        if pronto { return .navegando }
        if codigoDNS == -65570 { return .bloqueado } // kDNSServiceErr_PolicyDenied, TN3179
        return falhou ? .falha : .aguardando
    }
}
