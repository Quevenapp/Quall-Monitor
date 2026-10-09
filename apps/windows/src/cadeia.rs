//! **A condenação da cadeia de referência**, e a porta que ela abre e fecha.
//!
//! É a peça que `docs/contrato-track.md` fixa para toda casca receptora, e a casca do Windows era a
//! última sem ela. As outras cinco a têm: `ReceptorSessao.kt` (Android), `SessaoDeRecepcao.swift`
//! (iOS e app macOS), `Receptor.swift` (câmera do macOS) e `receptor.c` (plugin do OBS). Os nomes
//! dos cinco contadores são **literais** do contrato e não variam entre cascas.
//!
//! # O que ela testemunha, e por que nenhum contador de antes testemunhava
//!
//! Todo contador desta casca conta **entrega**: `recebidos`, `submetidos`, `apresentados`,
//! `pacotes_faltando`, `quadros_descartados`. Nenhum contava se a imagem entregue está **certa**.
//! Entre uma ruptura da cadeia — o núcleo descartou um quadro incompleto — e o IDR seguinte, todo
//! quadro P que chega depende de uma referência que não existe: ele chega inteiro, decodifica sem
//! erro nenhum e sai visualmente podre. Conta como sucesso em todas as linhas.
//!
//! Era o que o usuário via na bancada: *"a imagem está falhando"* contra *"0 falhas"*. Numa corrida
//! A10s → iPad com 1,6 % de perda real, **301 de 1156 quadros — 26 % da sessão — foram para a tela
//! com a referência quebrada** enquanto quatro contadores diziam zero.
//!
//! `suspeitos` é **dedução da estrutura de referência do H.264**, não medida óptica: ele responde
//! *"este quadro tinha como estar certo?"*, e não *"este quadro está certo?"*.
//!
//! # Nesta casca a condenação é **exata, por quadro** — e isso é vantagem do desenho, não sorte
//!
//! Nas cascas Apple e na do OBS a condenação entra por laço de amostragem (20 Hz no iOS, 50 ms no
//! OBS) e `suspeitos` é um **piso**: o núcleo despacha o quadro com o cadeado do depacotizador na
//! mão, então ler contador do núcleo de dentro do tratador de quadro fecharia um ciclo ABBA.
//!
//! Aqui o caminho do quadro passa pelo **laço da sessão**: o tratador da libdatachannel só empurra
//! uma cópia no canal, e quem tira, classifica e submete é a thread da sessão, que já lia
//! `TrackReceptor::contadores()` na mesma volta. A leitura já acontece fora do tratador, e por isso
//! **cada quadro é classificado com uma leitura própria de contadores**, feita no instante em que
//! ele sai da fila. É a granularidade do Android, e pelo mesmo motivo: quem tira o quadro é quem
//! pergunta.
//!
//! O resíduo que **sobra**, e ele é honesto: o quadro tirado agora pode ter entrado na fila
//! (`FILA_DE_QUADROS`, oito posições) algumas dezenas de milissegundos atrás, e
//! um descarte ocorrido **depois** dele já aparece nesta leitura. O erro, então, é para o lado de
//! condenar **cedo demais** — o oposto do iOS, que condena tarde e subestima. `recebidos` e
//! `transbordos` na linha de relato dizem o tamanho desse resíduo em cada corrida.
//!
//! # As duas origens de ruptura, e a segunda o núcleo nunca vai relatar
//!
//! 1. **`quadros_descartados` do núcleo sobe** — o depacotizador jogou fora um quadro incompleto.
//!    É o mesmo fato que já disparava o pedido de IDR nesta casca, lido de um segundo jeito.
//! 2. **A fila entre a thread da rede e a da sessão transbordou.** Estes quadros chegaram
//!    inteiros; quem os jogou fora fomos nós, porque o laço não deu conta de tirá-los. Para o
//!    decodificador o efeito é idêntico — o quadro seguinte referencia algo que nunca foi
//!    decodificado. O Android mediu 18 rupturas dessa origem ao lado de 126 do núcleo numa corrida
//!    só; sem esta linha elas ficariam fora de `rupturas` e os quadros seguintes fora de
//!    `suspeitos`. O `receptor.c` do OBS conta a mesma segunda origem, pela mesma razão.
//!
//! # A porta, e por que ela nasce desligada
//!
//! O quadro condenado é **decodificado assim mesmo** — parar de alimentar o decodificador
//! dessincroniza a sessão e faz o IDR seguinte chegar num decoder com buraco — e **não é
//! apresentado**: a tela segura o último quadro bom até a cadeia se curar. Quem faz isso é
//! `Exibicao::bombear`; esta peça só marca o quadro.
//!
//! Ligada por padrão ela foi mostrada ao usuário e a palavra dele foi *"terrível"*: a tela ficava
//! parada, `fps 0,0`, e todos os intervalos batendo na válvula. Por isso ela existe como
//! `--congelar-na-ruptura` e não como constante — esta bancada **mede** porta ligada contra
//! desligada em vez de argumentar.
//!
//! **Os contadores contam dos dois lados**: `suspeitos`, `rupturas`, `pior_rajada` e
//! `sem_referencia_ms` não dependem da porta; só `retidos` depende. Virar a chave muda o que se vê,
//! não o que se mede.

use std::time::{Duration, Instant};

/// Por quanto tempo, no máximo, a porta segura o último quadro bom esperando um IDR.
///
/// **A válvula.** Um emissor que aceita o pedido de IDR e não o atende existiu de verdade nesta
/// bancada — o `quall-app.exe` de 27/08 pôs **um** IDR na sessão inteira contra 54 pedidos. Contra
/// ele, congelar sem prazo trocaria imagem suja por imagem parada, que é pior. Mesmo valor das
/// outras cinco cascas.
pub const CONGELAR_NO_MAXIMO: Duration = Duration::from_secs(2);

/// Teto de amostras de `sem_referencia_ms` guardadas. Mesmo teto do Android, e pelo mesmo motivo:
/// a lista existe para dar percentil no fim da sessão, não para crescer sem limite numa sessão de
/// horas.
const MAXIMO_DE_AMOSTRAS: usize = 4000;

/// A máquina da condenação. Vive na thread da sessão, como tudo o mais deste receptor.
pub struct Condenacao {
    /// A porta está ligada? Ver o cabeçalho do módulo: ela nasce **desligada**.
    congelar: bool,
    condenada: bool,
    /// Quando a condenação começou. `None` quando a cadeia está sã.
    desde: Option<Instant>,
    /// Linha de base de `quadros_descartados`, **só** da condenação — separada da de
    /// `PoliticaDeIdr` de propósito: aquela soma `pacotes_perdidos()` junto e
    /// responde outra pergunta, com outro relógio.
    descartados: Option<u64>,
    /// Linha de base dos transbordos da fila local. Ver a segunda origem, no cabeçalho.
    transbordos: Option<u64>,
    /// Quantas vezes a cadeia de referência foi quebrada.
    pub rupturas: u64,
    /// Quadros chegados **depois** de uma ruptura e **antes** do IDR seguinte.
    pub suspeitos: u64,
    suspeitos_na_rajada: u64,
    pior_rajada: u64,
    /// Quantas vezes a válvula abriu a porta sem IDR. Não é do contrato; é o que separa
    /// "a cadeia se curou" de "desistimos de esperar" ao ler um `sem_referencia_ms` alto.
    pub aberturas_pela_valvula: u64,
    sem_referencia_ms: Vec<f64>,
}

impl Condenacao {
    pub fn nova(congelar: bool) -> Self {
        Condenacao {
            congelar,
            condenada: false,
            desde: None,
            descartados: None,
            transbordos: None,
            rupturas: 0,
            suspeitos: 0,
            suspeitos_na_rajada: 0,
            pior_rajada: 0,
            aberturas_pela_valvula: 0,
            sem_referencia_ms: Vec::new(),
        }
    }

    /// Olha as duas origens de ruptura e condena a cadeia quando **qualquer uma** sobe.
    ///
    /// A primeira chamada só fixa as linhas de base: uma sessão que já descartou alguma coisa antes
    /// de esta peça existir não é ruptura desta peça.
    ///
    /// Devolve `true` quando houve ruptura nesta volta — quem chama não precisa do valor, mas os
    /// testes precisam, e um `bool` é mais barato de checar que um contador.
    pub fn notar_contadores(
        &mut self,
        quadros_descartados: u64,
        transbordos: u64,
        agora: Instant,
    ) -> bool {
        let mut rompeu = false;
        match self.descartados {
            None => self.descartados = Some(quadros_descartados),
            Some(antes) if quadros_descartados > antes => {
                self.descartados = Some(quadros_descartados);
                rompeu = true;
            }
            // Contador que regride é leitura degradada, não conserto: a linha de base fica onde
            // está, e a próxima subida de verdade volta a acusar.
            Some(_) => {}
        }
        match self.transbordos {
            None => self.transbordos = Some(transbordos),
            Some(antes) if transbordos > antes => {
                self.transbordos = Some(transbordos);
                rompeu = true;
            }
            Some(_) => {}
        }
        if rompeu {
            self.rupturas += 1;
            if !self.condenada {
                self.desde = Some(agora);
            }
            self.condenada = true;
        }
        rompeu
    }

    /// Classifica **um** quadro. Devolve `true` quando a porta deve segurá-lo.
    ///
    /// A ordem aqui é a do Android e a do OBS, e ela não é estilo:
    ///
    /// 1. **o IDR cura** — ele é o quadro que não depende de referência nenhuma, venha do pedido de
    ///    IDR ou do GOP do emissor. Fecha a rajada e o intervalo sem referência;
    /// 2. um quadro P com a cadeia condenada é **contado** como suspeito, com a porta ligada ou
    ///    desligada;
    /// 3. **a válvula é conferida depois de contar**, então o quadro que a abre entra em
    ///    `suspeitos` (ele era suspeito) e **não** é segurado (a porta já abriu). É o
    ///    comportamento das outras cascas, literalmente.
    pub fn classificar(&mut self, idr: bool, agora: Instant) -> bool {
        if idr {
            if let Some(desde) = self.desde {
                self.anotar_intervalo(agora.saturating_duration_since(desde));
            }
            self.pior_rajada = self.pior_rajada.max(self.suspeitos_na_rajada);
            self.suspeitos_na_rajada = 0;
            self.condenada = false;
            self.desde = None;
            return false;
        }
        if !self.condenada {
            return false;
        }
        self.suspeitos += 1;
        self.suspeitos_na_rajada += 1;
        if let Some(desde) = self.desde {
            let sem_referencia = agora.saturating_duration_since(desde);
            if sem_referencia > CONGELAR_NO_MAXIMO {
                // O intervalo entra na conta do mesmo jeito: ele não terminou porque a imagem se
                // curou, terminou porque desistimos de esperar, e esconder o pior caso é o oposto
                // do que este contador existe para fazer.
                self.anotar_intervalo(sem_referencia);
                self.aberturas_pela_valvula += 1;
                self.condenada = false;
                self.desde = None;
                // **A rajada não é fechada aqui**, e isto é cópia deliberada do Android e do OBS:
                // a válvula abre a porta, não cura a cadeia. `pior_rajada()` já publica o maior
                // entre o guardado e o corrente, então nada se perde.
            }
        }
        self.condenada && self.congelar
    }

    /// O maior rastro de suspeitos seguidos, **incluindo a rajada que ainda está aberta**.
    pub fn pior_rajada(&self) -> u64 {
        self.pior_rajada.max(self.suspeitos_na_rajada)
    }

    /// A cadeia está condenada agora?
    pub fn condenada(&self) -> bool {
        self.condenada
    }

    fn anotar_intervalo(&mut self, quanto: Duration) {
        if self.sem_referencia_ms.len() < MAXIMO_DE_AMOSTRAS {
            self.sem_referencia_ms.push(quanto.as_secs_f64() * 1000.0);
        }
    }

    /// `sem_referencia_ms` como `[n=… p50=… p95=… max=…]` — o formato das outras cascas, para um
    /// roteiro de bancada ler as seis com um parser só.
    pub fn sem_referencia_ms(&self) -> String {
        if self.sem_referencia_ms.is_empty() {
            return "[n=0 p50=0 p95=0 max=0]".into();
        }
        let mut v = self.sem_referencia_ms.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p = |q: f64| -> f64 {
            let i = ((q * (v.len() - 1) as f64) as usize).min(v.len() - 1);
            v[i]
        };
        format!(
            "[n={} p50={:.0} p95={:.0} max={:.0}]",
            v.len(),
            p(0.50),
            p(0.95),
            v[v.len() - 1]
        )
    }

    /// Os cinco nomes do contrato, na ordem da tabela, para o diário.
    ///
    /// `retidos` chega de fora porque a porta mora em `exibicao::Contadores`: quem sabe
    /// quantos quadros não foram apresentados é quem apresenta.
    pub fn linha(&self, retidos: u64) -> String {
        format!(
            "cadeia: rupturas={} suspeitos={} pior_rajada={} retidos={} sem_referencia_ms={} \
             congelar={} aberturas_pela_valvula={}",
            self.rupturas,
            self.suspeitos,
            self.pior_rajada(),
            retidos,
            self.sem_referencia_ms(),
            if self.congelar { "sim" } else { "NAO" },
            self.aberturas_pela_valvula,
        )
    }

    /// A linha que vai para a **tela**, com os cinco números e `retidos` vindo da exibição.
    ///
    /// Existe separada de [`Self::linha`] porque `retidos` é contador de
    /// `exibicao::Contadores` — a porta mora lá — e porque esta é a superfície que o
    /// contrato cobra: *"o painel mostra os cinco, com destaque visual quando `suspeitos > 0`"*.
    /// Em 31/08/2026, com os contadores já certos no diário do receptor iOS, o usuário olhou para
    /// o iPad no meio de uma corrida com 124 quadros suspeitos e disse *"continua falhando e 0
    /// falhas"*. Ele estava certo: a queixa sempre foi sobre a tela.
    pub fn linha_da_tela(&self, retidos: u64) -> String {
        format!(
            "imagem: rupturas {} · suspeitos {} · pior rajada {} · retidos {} · sem_referencia_ms {}",
            self.rupturas,
            self.suspeitos,
            self.pior_rajada(),
            retidos,
            self.sem_referencia_ms(),
        )
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn a_primeira_leitura_so_fixa_a_linha_de_base() {
        let mut c = Condenacao::nova(true);
        // Uma sessão que já descartou sete quadros antes de esta peça olhar não teve sete rupturas
        // agora: a primeira chamada ancora e não acusa.
        assert!(!c.notar_contadores(7, 3, t0()));
        assert_eq!(c.rupturas, 0);
        assert!(!c.condenada());
    }

    #[test]
    fn a_subida_do_descarte_do_nucleo_condena() {
        let a = t0();
        let mut c = Condenacao::nova(true);
        c.notar_contadores(0, 0, a);
        assert!(c.notar_contadores(1, 0, a));
        assert_eq!(c.rupturas, 1);
        assert!(c.condenada());
    }

    #[test]
    fn o_transbordo_da_fila_local_tambem_condena() {
        let a = t0();
        let mut c = Condenacao::nova(true);
        c.notar_contadores(0, 0, a);
        // O núcleo não mexeu; quem jogou o quadro fora fomos nós. Para o decodificador é a mesma
        // coisa, e o contrato conta a mesma ruptura.
        assert!(c.notar_contadores(0, 1, a));
        assert_eq!(c.rupturas, 1);
        assert!(c.condenada());
    }

    #[test]
    fn uma_volta_com_as_duas_origens_subindo_e_uma_ruptura_so() {
        let a = t0();
        let mut c = Condenacao::nova(true);
        c.notar_contadores(0, 0, a);
        c.notar_contadores(4, 2, a);
        assert_eq!(c.rupturas, 1, "duas origens no mesmo instante são um evento");
    }

    #[test]
    fn o_quadro_p_depois_da_ruptura_e_suspeito_e_a_porta_o_segura() {
        let a = t0();
        let mut c = Condenacao::nova(true);
        c.notar_contadores(0, 0, a);
        c.notar_contadores(1, 0, a);
        assert!(c.classificar(false, a), "a porta ligada segura o suspeito");
        assert!(c.classificar(false, a));
        assert_eq!(c.suspeitos, 2);
        assert_eq!(c.pior_rajada(), 2);
    }

    #[test]
    fn com_a_porta_desligada_o_suspeito_conta_igual_e_vai_para_a_tela() {
        let a = t0();
        let mut c = Condenacao::nova(false);
        c.notar_contadores(0, 0, a);
        c.notar_contadores(1, 0, a);
        assert!(!c.classificar(false, a), "a porta desligada não segura nada");
        assert_eq!(c.suspeitos, 1, "e o contador conta do mesmo jeito");
    }

    #[test]
    fn o_idr_cura_a_cadeia_e_fecha_a_rajada() {
        let a = t0();
        let mut c = Condenacao::nova(true);
        c.notar_contadores(0, 0, a);
        c.notar_contadores(1, 0, a);
        c.classificar(false, a);
        c.classificar(false, a);
        c.classificar(false, a);
        assert!(!c.classificar(true, a + Duration::from_millis(90)), "o IDR nunca é segurado");
        assert!(!c.condenada());
        assert_eq!(c.pior_rajada(), 3);
        // E o quadro P seguinte, com a cadeia sã, não é suspeito.
        assert!(!c.classificar(false, a + Duration::from_millis(120)));
        assert_eq!(c.suspeitos, 3);
        assert_eq!(c.sem_referencia_ms(), "[n=1 p50=90 p95=90 max=90]");
    }

    #[test]
    fn uma_segunda_rajada_menor_nao_apaga_a_pior() {
        let a = t0();
        let mut c = Condenacao::nova(true);
        c.notar_contadores(0, 0, a);
        c.notar_contadores(1, 0, a);
        for _ in 0..5 {
            c.classificar(false, a);
        }
        c.classificar(true, a);
        c.notar_contadores(2, 0, a);
        c.classificar(false, a);
        c.classificar(true, a);
        assert_eq!(c.pior_rajada(), 5);
        assert_eq!(c.rupturas, 2);
        assert_eq!(c.suspeitos, 6);
    }

    #[test]
    fn a_valvula_abre_a_porta_passado_o_teto_mesmo_sem_idr() {
        let a = t0();
        let mut c = Condenacao::nova(true);
        c.notar_contadores(0, 0, a);
        c.notar_contadores(1, 0, a);
        assert!(c.classificar(false, a + Duration::from_millis(1900)), "ainda dentro do teto");
        // Passado o teto: o quadro é contado como suspeito e **não** é segurado.
        assert!(!c.classificar(false, a + CONGELAR_NO_MAXIMO + Duration::from_millis(1)));
        assert!(!c.condenada());
        assert_eq!(c.suspeitos, 2, "o quadro que abriu a válvula era suspeito e conta");
        assert_eq!(c.aberturas_pela_valvula, 1);
        // O intervalo que a válvula fechou entra na conta: esconder o pior caso seria o oposto do
        // que este contador existe para fazer.
        assert_eq!(c.sem_referencia_ms(), "[n=1 p50=2001 p95=2001 max=2001]");
    }

    #[test]
    fn depois_da_valvula_uma_ruptura_nova_condena_de_novo() {
        let a = t0();
        let mut c = Condenacao::nova(true);
        c.notar_contadores(0, 0, a);
        c.notar_contadores(1, 0, a);
        c.classificar(false, a + CONGELAR_NO_MAXIMO + Duration::from_millis(1));
        assert!(!c.condenada());
        c.notar_contadores(2, 0, a + Duration::from_secs(3));
        assert!(c.condenada());
        assert!(c.classificar(false, a + Duration::from_secs(3)));
    }

    #[test]
    fn sem_ruptura_nenhuma_nada_e_suspeito() {
        let a = t0();
        let mut c = Condenacao::nova(true);
        c.notar_contadores(0, 0, a);
        for i in 0..30 {
            assert!(!c.classificar(i == 0, a));
        }
        assert_eq!(c.suspeitos, 0);
        assert_eq!(c.rupturas, 0);
        assert_eq!(c.sem_referencia_ms(), "[n=0 p50=0 p95=0 max=0]");
    }

    #[test]
    fn os_percentis_saem_do_conjunto_de_amostras_e_nao_de_uma_media() {
        let a = t0();
        let mut c = Condenacao::nova(true);
        c.notar_contadores(0, 0, a);
        // Dez intervalos: nove de 10 ms e um de 1000 ms. Uma média diria 109 ms e esconderia o
        // segundo inteiro de tela errada, que é justamente o que a pessoa sente.
        for (i, ms) in [10u64, 10, 10, 10, 10, 10, 10, 10, 10, 1000].iter().enumerate() {
            let base = a + Duration::from_secs(10 * (i as u64 + 1));
            c.notar_contadores(i as u64 + 1, 0, base);
            c.classificar(true, base + Duration::from_millis(*ms));
        }
        // `p95` de dez amostras cai no índice 8 (`0.95 × 9 = 8,55`), que ainda é 10 ms — a mesma
        // aritmética de percentil do Android, de propósito. Quem denuncia o segundo inteiro de
        // tela errada é o `max`, e é por isso que ele vai na linha.
        assert_eq!(c.sem_referencia_ms(), "[n=10 p50=10 p95=10 max=1000]");
    }
}
