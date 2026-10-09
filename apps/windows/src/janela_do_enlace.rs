//! **O caminho de volta do sinal**: o dano do enlace numa *janela*, e não desde o começo da sessão.
//!
//! Todo contador de recepção deste projeto é acumulado desde o início — `pacotes_vistos`,
//! `pacotes_perdidos_de_verdade`, `idrs_quebrados`, e o `suspeitos` que esta casca conta sozinha
//! (ver [`crate::cadeia`]). Isso é certo para o relato final e é inútil para decidir alguma coisa
//! **agora**: uma sessão que perdeu 8 % nos primeiros dez segundos e nada depois continua dizendo
//! 8 % meia hora adiante. Quem escuta o enlace precisa da derivada, não da integral.
//!
//! # Por que esta peça existe, e por que ela não existia no Windows
//!
//! O controlador de taxa do emissor (`quall_core::taxa`) é alimentado por
//! `Ready::relatar_enlace`, que **o receptor** chama. Sem esta peça, uma
//! casca receptora nunca manda amostra e o controlador do outro lado é **inerte por construção** —
//! não "desligado": ele roda e não tem o que ler. Foi o que a bancada mediu em 31/08/2026 no par
//! A10s → iPad, com o controlador ligado: `trocas_de_bitrate=0` com 2,95 % de perda e 881 quadros
//! exibidos com a referência quebrada, porque o relato existia **só** na casca Android.
//!
//! Relatavam Android, iOS, app macOS, câmera do macOS e o plugin do OBS. O receptor do Windows era
//! a última casca muda. É a mesma peça daquelas cinco — `JanelaDoEnlace.kt`, `JanelaDoEnlace.swift`
//! e `janela-do-enlace.c` —, com a mesma forma e os mesmos cinco nomes, **de propósito**: um
//! controlador alimentado por um número diferente do que a bancada mediu é um controlador projetado
//! contra outra curva.
//!
//! # Por que ela mora em `apps/windows` e não em `crates/quall-core`
//!
//! Ela é **política de casca**: lê `suspeitos`, que é contador da casca e não do núcleo. As outras
//! cinco cascas têm a sua, e promover justo a versão Rust para o núcleo faria "a mesma peça em
//! toda casca" deixar de ser verdade de um jeito fácil de não perceber — cinco cópias e uma
//! biblioteca. Se um segundo consumidor Rust aparecer, aí se promove.
//!
//! Pelo mesmo motivo do `janela-do-enlace.c` do OBS, este módulo **não toca `quall-core`**: ele
//! recebe `u64` e devolve `u64`. É o que torna a aritmética exercitável sem rede, sem aparelho e
//! sem o `webrtc` do núcleo — a única prova que uma frente sem bancada tem como produzir.
//!
//! # O denominador vem do emissor, e isso não é detalhe
//!
//! [`Amostra::pacotes`] é `vistos + perdidos`, que é **o que o emissor mandou** na janela — os dois
//! termos saem de números de sequência RTP, que são contíguos. Dividir a perda pelo que **chegou**
//! responde outra pergunta, e o viés não é constante: numa medição desta bancada ele inverteu a
//! ordem entre dois braços da matriz com o laudo já escrito. O denominador é montado **aqui
//! dentro**, e não por quem lê a linha, para não haver onde errar.
//!
//! [`Amostra::perdidos`] é `pacotes_perdidos_de_verdade` e **nunca** `pacotes_faltando`. Aquele é o
//! teto (`packets_missing_upper_bound` na fronteira C): ele cobra reordenação como perda, com erro
//! medido de 1,3× a 44×, e um controlador alimentado por ele reduziria o bitrate por causa de
//! pacotes que chegaram.
//!
//! # Período de 500 ms, e ele não é escolha livre
//!
//! É a janela contra a qual a política do controlador foi medida (`crates/quall-core/src/taxa.rs`,
//! `docs/taxa-que-escuta.md` §§2 e 4). Alimentar aquele controlador com outra janela é projetá-lo
//! contra outra curva. É a mesma constante das outras cinco cascas.

use std::time::{Duration, Instant};

/// O período da janela. Ver o cabeçalho: **não** é escolha livre.
pub const PERIODO: Duration = Duration::from_millis(500);

/// Os acumulados que uma janela precisa, lidos **num instante só**.
///
/// Os três primeiros são do núcleo (`quall_core::rtp::Contadores`); `suspeitos` é da casca. Quem
/// os converte é `receptor.rs`, que é quem sabe de qual thread isso pode ser lido.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Acumulados {
    /// `pacotes_vistos`.
    pub vistos: u64,
    /// `pacotes_perdidos_de_verdade` — a perda **exata**, nunca o teto `pacotes_faltando`.
    pub perdidos: u64,
    /// `idrs_quebrados`.
    pub idrs_quebrados: u64,
    /// O contador desta casca. Ver `crate::cadeia::Condenacao`.
    pub suspeitos: u64,
}

/// Uma janela fechada. **Todos os campos são deltas da janela**, exceto [`Amostra::ms`]; nenhum é
/// acumulado desde o começo da sessão. Os cinco nomes são os de
/// `quall_core::signaling::RelatoDoEnlace` e são literais.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Amostra {
    /// Duração **real** da janela, em ms. Nunca a nominal: esta peça é chamada de um laço que
    /// acorda quando acorda, e dividir pelo período nominal daria uma taxa sistematicamente alta.
    pub ms: u64,
    /// O que o emissor mandou nesta janela: `vistos + perdidos`. Ver o cabeçalho do módulo.
    pub pacotes: u64,
    /// Perda **exata** na janela.
    pub perdidos: u64,
    /// Quadros condenados na janela.
    pub suspeitos: u64,
    /// `idrs_quebrados` na janela.
    pub idrs_quebrados: u64,
}

impl Amostra {
    /// Perda da janela em por cento, com o denominador do emissor.
    ///
    /// `pacotes == 0` sai como `0` e não como divisão por zero: sem nada no ar não há taxa a
    /// afirmar, e a janela existe assim mesmo — meio segundo em que o emissor não mandou nada é
    /// informação.
    pub fn perda_pct(&self) -> f64 {
        if self.pacotes == 0 {
            0.0
        } else {
            self.perdidos as f64 * 100.0 / self.pacotes as f64
        }
    }

    /// A linha do diário, no **mesmo** formato das outras cinco cascas, para um roteiro de bancada
    /// ler as seis com um parser só:
    ///
    /// ```text
    /// janela_do_enlace ms=502 pacotes=1000 perdidos=30 (3.00%) suspeitos=2 idrs_quebrados=1
    /// ```
    pub fn linha(&self) -> String {
        format!(
            "janela_do_enlace ms={} pacotes={} perdidos={} ({:.2}%) suspeitos={} idrs_quebrados={}",
            self.ms,
            self.pacotes,
            self.perdidos,
            self.perda_pct(),
            self.suspeitos,
            self.idrs_quebrados,
        )
    }
}

/// A âncora da janela aberta. Sem estado global e sem alocação: quem chama guarda uma destas na
/// pilha do laço que é dono da sessão.
pub struct JanelaDoEnlace {
    aberta_em: Option<Instant>,
    ancora: Acumulados,
    /// Quantas vezes uma leitura foi **recusada** por regredir. Ver [`Self::fechar`].
    pub leituras_recusadas: u64,
}

impl Default for JanelaDoEnlace {
    fn default() -> Self {
        Self::nova()
    }
}

impl JanelaDoEnlace {
    pub fn nova() -> Self {
        JanelaDoEnlace {
            aberta_em: None,
            ancora: Acumulados::default(),
            leituras_recusadas: 0,
        }
    }

    /// Fecha a janela se ela já durou `periodo`, devolve a **derivada** e reancora.
    ///
    /// Devolve `None` — sem tocar na âncora — em quatro casos, e nenhum deles é erro:
    ///
    /// 1. **a primeira chamada**, que só ancora. Os acumulados de uma sessão que já rodou meio
    ///    segundo antes de a primeira janela abrir não são dano desta janela: uma primeira janela
    ///    contando desde zero mediria o arranque da sessão (o primeiro IDR, a subida do ICE) como
    ///    se fosse regime, e o controlador do outro lado veria uma perda que já tinha passado;
    /// 2. **a janela ainda não fechou** (`decorrido < periodo`);
    /// 3. `periodo` zero, que não é janela nenhuma;
    /// 4. **a leitura regrediu** — ver abaixo.
    ///
    /// # Leitura falha do núcleo custa um tique, e nunca vira zero
    ///
    /// As quatro cascas anteriores concordam nisto desde 01/09/2026, e a razão é aritmética:
    /// fechar a janela com zeros **zera a âncora**, e a janela seguinte entrega como dano de 500 ms
    /// tudo o que a sessão acumulou desde o começo — a integral entrando pela porta que existe
    /// justamente para dar a derivada. Um zero em `perdidos` não fica num diário: ele atravessa até
    /// o emissor como *"medi e não perdi nada"* e faz o controlador **subir** o bitrate.
    ///
    /// Nas cascas C e Swift a leitura falha é visível — falta uma chave no JSON. **Aqui ela é
    /// muda**: `TrackReceptor::contadores()` é infalível na assinatura e não na prática, porque com
    /// o cadeado do depacotizador envenenado ela devolve `Contadores::default()`, todos os campos
    /// em zero, sem erro nenhum. A assinatura desse caminho é uma leitura **menor que a âncora**, e
    /// é por isso que a recusa mora aqui: nesta casca a `TrackReceptor` vive numa variável só, do
    /// início ao fim da sessão, e **nunca é recriada** — logo um acumulado do núcleo que anda para
    /// trás não é track nova, é leitura degradada.
    ///
    /// Os deltas continuam sendo **saturantes** de qualquer forma, e não por desconfiança do
    /// compilador: `suspeitos` é contador da casca e não passa pela recusa acima, e uma janela
    /// zerada custa meio segundo de silêncio enquanto uma janela absurda (`0 − 130` em `u64` são
    /// dezoito quintilhões) custa a decisão do controlador.
    pub fn fechar(
        &mut self,
        agora: Instant,
        periodo: Duration,
        leitura: Acumulados,
    ) -> Option<Amostra> {
        if periodo.is_zero() {
            return None;
        }
        let Some(aberta_em) = self.aberta_em else {
            self.ancorar(agora, leitura);
            return None;
        };
        if leitura.vistos < self.ancora.vistos
            || leitura.perdidos < self.ancora.perdidos
            || leitura.idrs_quebrados < self.ancora.idrs_quebrados
        {
            self.leituras_recusadas += 1;
            return None;
        }
        let decorrido = agora.saturating_duration_since(aberta_em);
        if decorrido < periodo {
            return None;
        }

        let dv = leitura.vistos.saturating_sub(self.ancora.vistos);
        let dp = leitura.perdidos.saturating_sub(self.ancora.perdidos);
        let amostra = Amostra {
            ms: decorrido.as_millis() as u64,
            // **O denominador do emissor**, montado aqui e não por quem lê a linha.
            pacotes: dv.saturating_add(dp),
            perdidos: dp,
            suspeitos: leitura.suspeitos.saturating_sub(self.ancora.suspeitos),
            idrs_quebrados: leitura
                .idrs_quebrados
                .saturating_sub(self.ancora.idrs_quebrados),
        };
        self.ancorar(agora, leitura);
        Some(amostra)
    }

    fn ancorar(&mut self, agora: Instant, leitura: Acumulados) {
        self.aberta_em = Some(agora);
        self.ancora = leitura;
    }
}

/// A linha de perda **mastigada**, com os três números que a perda precisa para não mentir.
///
/// Mora aqui, ao lado da janela, porque é a outra leitura que esta casca faz do mesmo enlace, e
/// porque ela também não precisa de `quall-core` para ser exercitada.
///
/// # A dívida que ela paga
///
/// `docs/contador-nas-cascas.md` §6 registra, com nome e endereço: *"`apps/windows/src/receptor.rs`
/// imprime `pacotes_faltando=` e não mostra o contador exato… continua sendo a quinta casca
/// receptora mostrando só o teto"*. O teto é a soma dos saltos de sequência, e uma reordenação de
/// distância `d` entra nele como `1 + d` posições sem que nada tenha se perdido: numa corrida de
/// 29/08 ele marcava **486** onde a perda real eram **50**. O erro medido vai de 1,3× a 44×, e
/// esse número foi lido como perda em toda medição desta bancada.
///
/// A linha sai **byte a byte** igual à das outras quatro cascas, e isso é o ponto — três formatos
/// para o mesmo número seriam meio caminho para o próximo mal-entendido:
///
/// ```text
/// perda exata 50 (0.180%) · teto 486 (1.722%) · tarde demais 0 · vistos 27729
/// ```
///
/// O denominador é `contador + vistos` — a janela observada mais o que faltou nela. Nada que tenha
/// caído antes do primeiro pacote visto pode entrar em conta nenhuma, e é para isso que
/// `pacotes_vistos` existe.
///
/// `tarde_demais` diferente de zero acrescenta um aviso em maiúsculas: a janela de reordenação de
/// 128 posições foi curta, e a perda exata está superestimada nesse tanto. Sem isso ele seria mais
/// um número mudo no meio de uma linha — que é exatamente como o teto enganou.
pub fn resumo_de_perda(exata: u64, teto: u64, tarde_demais: u64, vistos: u64) -> String {
    if vistos == 0 {
        return "perda: nenhum pacote chegou ainda (pacotes_vistos=0) — nada a afirmar".into();
    }
    let taxa = |v: u64| -> String {
        let den = (v + vistos) as f64;
        format!("{} ({:.3}%)", v, if den > 0.0 { 100.0 * v as f64 / den } else { 0.0 })
    };
    let mut linha = format!(
        "perda exata {} · teto {} · tarde demais {} · vistos {}",
        taxa(exata),
        taxa(teto),
        tarde_demais,
        vistos,
    );
    if tarde_demais > 0 {
        linha.push_str(&format!(
            " — JANELA CURTA: a perda exata está superestimada em até {tarde_demais}"
        ));
    }
    linha
}

#[cfg(test)]
mod testes {
    use super::*;

    fn acum(vistos: u64, perdidos: u64, suspeitos: u64, idrs: u64) -> Acumulados {
        Acumulados { vistos, perdidos, idrs_quebrados: idrs, suspeitos }
    }

    #[test]
    fn a_primeira_chamada_so_ancora() {
        let a = Instant::now();
        let mut j = JanelaDoEnlace::nova();
        assert_eq!(j.fechar(a, PERIODO, acum(4000, 120, 7, 2)), None);
    }

    #[test]
    fn a_janela_que_ainda_nao_fechou_nao_relata() {
        let a = Instant::now();
        let mut j = JanelaDoEnlace::nova();
        j.fechar(a, PERIODO, acum(0, 0, 0, 0));
        assert_eq!(
            j.fechar(a + Duration::from_millis(499), PERIODO, acum(500, 5, 1, 0)),
            None
        );
    }

    #[test]
    fn periodo_zero_nao_e_janela() {
        let a = Instant::now();
        let mut j = JanelaDoEnlace::nova();
        assert_eq!(j.fechar(a, Duration::ZERO, acum(0, 0, 0, 0)), None);
        assert_eq!(
            j.fechar(a + Duration::from_secs(10), Duration::ZERO, acum(9999, 99, 9, 9)),
            None
        );
    }

    #[test]
    fn a_derivada_com_o_denominador_do_emissor() {
        let a = Instant::now();
        let mut j = JanelaDoEnlace::nova();
        j.fechar(a, PERIODO, acum(10_000, 100, 40, 3));
        let am = j
            .fechar(a + Duration::from_millis(502), PERIODO, acum(10_970, 130, 42, 4))
            .expect("a janela fechou");
        assert_eq!(am.ms, 502, "a duração REAL, nunca a nominal");
        // 970 vistos + 30 perdidos = 1000 postos no ar pelo emissor.
        assert_eq!(am.pacotes, 1000);
        assert_eq!(am.perdidos, 30);
        assert_eq!(am.suspeitos, 2);
        assert_eq!(am.idrs_quebrados, 1);
        // 3,00 % com o denominador certo. O denominador errado — o que **chegou** — diria
        // 30/970 = 3,09 %, e foi um viés desse tamanho que inverteu a ordem de dois braços de uma
        // matriz desta bancada com o laudo já escrito.
        assert!((am.perda_pct() - 3.0).abs() < 1e-9, "{}", am.perda_pct());
        let vies_do_denominador_errado: f64 = 30.0 * 100.0 / 970.0;
        assert!(vies_do_denominador_errado - 3.0 > 0.05, "3,09 % contra 3,00 %: pequeno, e não nulo");
        assert_eq!(
            am.linha(),
            "janela_do_enlace ms=502 pacotes=1000 perdidos=30 (3.00%) suspeitos=2 idrs_quebrados=1"
        );
    }

    #[test]
    fn os_numeros_sao_deltas_e_nunca_acumulados() {
        let a = Instant::now();
        let mut j = JanelaDoEnlace::nova();
        j.fechar(a, PERIODO, acum(0, 0, 0, 0));
        let primeira = j
            .fechar(a + Duration::from_millis(500), PERIODO, acum(1000, 80, 10, 1))
            .expect("fechou");
        assert_eq!(primeira.perdidos, 80);
        // Nada de novo se perdeu na janela seguinte. Uma casca que relatasse o acumulado diria
        // 80 de novo, e o controlador do outro lado continuaria cortando bitrate meia hora depois
        // por causa de uma perda que já passou.
        let segunda = j
            .fechar(a + Duration::from_millis(1000), PERIODO, acum(2000, 80, 10, 1))
            .expect("fechou");
        assert_eq!(segunda.perdidos, 0);
        assert_eq!(segunda.pacotes, 1000);
        assert_eq!(segunda.suspeitos, 0);
        assert_eq!(segunda.idrs_quebrados, 0);
    }

    #[test]
    fn leitura_degradada_do_nucleo_nao_fecha_a_janela_e_nao_mexe_na_ancora() {
        let a = Instant::now();
        let mut j = JanelaDoEnlace::nova();
        j.fechar(a, PERIODO, acum(10_000, 100, 40, 3));
        // `Contadores::default()`: o que `TrackReceptor::contadores()` devolve com o cadeado
        // envenenado. Não é uma sessão que perdeu tudo — é uma leitura que não aconteceu.
        assert_eq!(
            j.fechar(a + Duration::from_millis(600), PERIODO, acum(0, 0, 0, 0)),
            None
        );
        assert_eq!(j.leituras_recusadas, 1);
        // E a âncora continua onde estava: a janela seguinte mede em cima dela, não em cima do
        // zero. Se a âncora tivesse sido zerada, esta janela entregaria 10.970 pacotes e 130
        // perdidos como dano de 1,2 s — a integral da sessão inteira entrando pela porta que
        // existe para dar a derivada.
        let am = j
            .fechar(a + Duration::from_millis(1200), PERIODO, acum(10_970, 130, 42, 4))
            .expect("a janela seguinte fecha");
        assert_eq!(am.pacotes, 1000);
        assert_eq!(am.perdidos, 30);
        assert_eq!(am.ms, 1200, "e o `ms` diz que ela durou 1,2 s, que é a verdade");
    }

    #[test]
    fn contador_que_regride_nao_vira_perda_negativa() {
        // A defesa de dentro da aritmética, medida sem passar pela recusa acima: `suspeitos` é
        // contador da casca, e um delta negativo em `u64` seria 18.446.744.073.709.551.486.
        let a = Instant::now();
        let mut j = JanelaDoEnlace::nova();
        j.fechar(a, PERIODO, acum(1000, 10, 500, 1));
        let am = j
            .fechar(a + Duration::from_millis(500), PERIODO, acum(2000, 20, 3, 1))
            .expect("fechou");
        assert_eq!(am.suspeitos, 0, "satura em zero, não em perda negativa");
        assert_eq!(am.perdidos, 10);
    }

    #[test]
    fn a_janela_que_nao_fechou_nao_reancora() {
        let a = Instant::now();
        let mut j = JanelaDoEnlace::nova();
        j.fechar(a, PERIODO, acum(0, 0, 0, 0));
        // Meia dúzia de voltas do laço dentro do período. Se qualquer uma delas reancorasse, a
        // janela seguinte mediria só o último pedaço.
        for ms in [10u64, 120, 240, 360, 480] {
            assert_eq!(
                j.fechar(a + Duration::from_millis(ms), PERIODO, acum(ms * 2, ms / 10, 0, 0)),
                None
            );
        }
        let am = j
            .fechar(a + Duration::from_millis(520), PERIODO, acum(1040, 52, 0, 0))
            .expect("fechou");
        assert_eq!(am.pacotes, 1040 + 52, "a janela inteira, e não o último pedaço");
        assert_eq!(am.perdidos, 52);
    }

    #[test]
    fn sem_nada_no_ar_a_janela_existe_e_a_taxa_e_zero() {
        let a = Instant::now();
        let mut j = JanelaDoEnlace::nova();
        j.fechar(a, PERIODO, acum(700, 3, 0, 0));
        let am = j
            .fechar(a + Duration::from_millis(500), PERIODO, acum(700, 3, 0, 0))
            .expect("fechou");
        assert_eq!(am.pacotes, 0);
        assert_eq!(am.perda_pct(), 0.0);
        assert_eq!(
            am.linha(),
            "janela_do_enlace ms=500 pacotes=0 perdidos=0 (0.00%) suspeitos=0 idrs_quebrados=0"
        );
    }

    // --- a dívida do `pacotes_faltando` ---------------------------------------------------------

    #[test]
    fn a_linha_de_perda_sai_igual_a_das_outras_cascas() {
        // Os números são os da corrida de 29/08 que nomeou a dívida: o teto marcava 486 e a perda
        // real eram 50.
        assert_eq!(
            resumo_de_perda(50, 486, 0, 27_729),
            "perda exata 50 (0.180%) · teto 486 (1.722%) · tarde demais 0 · vistos 27729"
        );
    }

    #[test]
    fn tarde_demais_diferente_de_zero_avisa_em_maiusculas() {
        let l = resumo_de_perda(50, 486, 7, 27_729);
        assert!(l.contains("JANELA CURTA"), "{l}");
        assert!(l.contains("até 7"), "{l}");
    }

    #[test]
    fn sem_pacote_nenhum_nao_ha_taxa_a_afirmar() {
        assert!(resumo_de_perda(0, 0, 0, 0).contains("nada a afirmar"));
    }
}
