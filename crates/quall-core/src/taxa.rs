//! O controlador de taxa: a parte do Quall que **escuta** o enlace.
//!
//! Até 31/08/2026 este projeto não tinha nenhuma. Não havia uma linha no núcleo que reduzisse o
//! bitrate quando o enlace reclamava: o emissor mandava o que mandaram ele mandar, em 5 GHz ou
//! em 2,4 GHz, e as duas medidas do mesmo dia diziam que o enlace de 2,4 GHz não aguenta o que
//! estamos mandando.
//!
//! Este módulo é a política, e só ela. Ele não fala com o encoder, não fala com a rede e não
//! sabe o que é um `MediaCodec`. Entra [`Amostra`], sai `Option<u32>` — o bitrate novo, ou nada.
//! Mora no núcleo por dois motivos, e nenhum é arquitetural:
//!
//! 1. **Para ser testado.** As cascas Android não têm suíte de testes; `cargo test --workspace`
//!    tem. Um laço de realimentação sem teste determinístico é a coisa mais cara que se pode pôr
//!    no caminho quente, porque o modo de falha dele — oscilar — só aparece com o rádio na
//!    frente e desaparece quando alguém vai olhar.
//! 2. **Para ser um só.** Cinco cascas emitem vídeo neste projeto. Cinco políticas de taxa
//!    seriam cinco produtos.
//!
//! # A regra que faz o braço de aferição negativo passar por construção
//!
//! O controlador **nasce no teto**, e o teto é o bitrate que o produto usaria sem ele.
//! A subida de [`ControleDeTaxa::amostra`] nunca passa do teto. Logo:
//!
//! > **O controlador só sabe tirar, e devolver o que tirou.**
//!
//! Num enlace limpo ele nunca tira nada — em 5 GHz esta bancada mediu **0 perda em 9 003
//! pacotes**, e o A/B desta frente mediu **0 em 53 001**, com `setParameters` chamado **zero
//! vezes em 181 janelas de decisão** —, então ele fica no teto e a sessão é byte a byte a mesma
//! que sem ele. Isso não é uma promessa sobre a sintonia dos parâmetros: é
//! consequência de o teto ser o valor de produto. Um controlador que pudesse *subir* acima do
//! valor de produto precisaria provar que não estraga o enlace bom; este não pode fazê-lo.
//!
//! # A banda morta é o que impede a oscilação, e ela é larga de propósito
//!
//! Entre [`Politica::perda_baixa_pct`] e [`Politica::perda_alta_pct`] o controlador **não faz
//! nada**. Um controlador com um limiar só bate entre dois valores para sempre: desce porque a
//! perda passou de X, sobe porque a perda ficou abaixo de X, desce de novo.
//!
//! São **quatro** freios independentes contra esse modo de falha, e o terceiro só entrou depois
//! de ele acontecer em aparelho: dois limiares com folga entre eles; a carência de uma janela
//! depois de cada mudança; a exigência de **calmaria agregada** (e não de janelas limpas
//! contadas) para subir; e a paciência que dobra a cada descida.
//!
//! # A assimetria não é gosto: descer é multiplicativo, subir é aditivo
//!
//! Descer é caro de errar para o lado de baixo (imagem feia), e caríssimo de errar para o lado
//! de cima (imagem quebrada). Subir devagar e descer rápido é o que faz o tempo passado em
//! estado ruim ser curto. É a mesma assimetria do controle de congestionamento do TCP, e pela
//! mesma razão.
//!
//! # O que este controlador NÃO conserta, e é preciso dizer
//!
//! **Ele não salva a rajada que está acontecendo.** A rajada que destrói um IDR dura menos de um
//! segundo, e o caminho até o bitrate novo soma três esperas: fechar a janela do receptor (até
//! 500 ms), o relato atravessar e ser lido (até 200 ms), e o `setParameters` entrar na volta
//! seguinte do laço de dreno (≤10 ms). **Até ~1,2 s no pior caso**, e **só a primeira das três já
//! é da ordem da rajada inteira** — não há sintonia que conserte isso. Quem responde à rajada em
//! curso é o pedido de IDR, que já existe e tem piso de 100 ms.
//!
//! O que este controlador faz é **reduzir a chance da próxima**. Medido em 31/08/2026, A10s →
//! tablet em 2,4 GHz, oito degraus de 20 s numa sessão só: de 4000 kbps pedidos para 700, os
//! quadros exibidos com a referência quebrada caem de **190-202 para 16-17** por degrau. Esse é
//! o número, e ele é sobre a frequência do dano, não sobre a duração de um evento.

/// **O piso do controlador, em bps.** Abaixo disto ele para de descer e passa a dizer que o
/// enlace não dá — ver [`ControleDeTaxa::no_piso`] e [`Politica::piso_bps`], onde a procedência do
/// número está escrita.
///
/// É `pub` porque [`crate::teto`] precisa dele: um teto de taxa abaixo do piso do controlador não
/// é um teto, é um erro de aritmética, e o único jeito de garantir que os dois não se cruzem é um
/// deles não existir separado.
pub const PISO_BPS: u32 = 400_000;

/// O dano do enlace numa janela, visto pelo receptor.
///
/// Deltas, nunca acumulados desde o início da sessão: uma sessão que perdeu 8 % nos primeiros
/// dez segundos e nada depois continua dizendo 8 % meia hora adiante, e um controlador
/// alimentado com isso nunca mais sobe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Amostra {
    /// Duração real da janela, em ms.
    pub ms: u64,
    /// Pacotes que o **emissor mandou** nesta janela: vistos + perdidos de verdade.
    ///
    /// O denominador vem do emissor, e não do que chegou. Em 31/08/2026 esta bancada quase
    /// publicou a conclusão oposta sobre a perda de regime porque um instrumento dividia pelo
    /// que chegou: o braço que parecia o melhor da matriz era o pior.
    pub pacotes: u64,
    /// Perda **exata** (`packets_lost_for_real`), nunca o teto `packets_missing_upper_bound`.
    pub perdidos: u64,
    /// Quadros que foram para a tela com a referência condenada. Não decide nada hoje — ver
    /// [`ControleDeTaxa::amostra`] — e viaja porque é o número que fecha a frente.
    pub suspeitos: u64,
    /// Unidades de acesso IDR que chegaram truncadas.
    pub idrs_quebrados: u64,
    /// **Quadros que o receptor recebeu inteiros e não conseguiu entregar** — fila dele
    /// transbordando. Ver [`crate::signaling::RelatoDoEnlace::nao_decodificados`].
    pub nao_decodificados: u64,
}

impl Amostra {
    /// Perda da janela, em por cento, com o denominador do emissor.
    pub fn perda_pct(&self) -> f32 {
        if self.pacotes == 0 {
            0.0
        } else {
            self.perdidos as f32 * 100.0 / self.pacotes as f32
        }
    }
}

/// Os parâmetros da política. Todos com valor medido ou justificado; nenhum é chute mudo.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Politica {
    /// Bitrate máximo, em bps. **É o valor que o produto usaria sem controlador.**
    ///
    /// Ver o cabeçalho do módulo: é isto que faz o braço de aferição negativo passar por
    /// construção, e não a sintonia dos outros campos.
    pub teto_bps: u32,
    /// Bitrate mínimo, em bps. Abaixo disto o controlador para de descer e passa a dizer que o
    /// enlace não dá — ver [`ControleDeTaxa::no_piso`].
    ///
    /// **400 kbps, e o número tem procedência.** A 720x1520 e 30 fps, a curva medida em
    /// 31/08/2026 entrega 350-360 kbps no fio para 300 kbps pedidos, com 56 pacotes/s e perda de
    /// 0,235 %: o enlace deixa de ser o problema bem antes disso. O que decide o piso não é a
    /// rede, é o olho — e o olho não é meu. 400 kbps é o menor degrau da escada medida em que a
    /// perda já está no chão, e ele existe para que a resposta a um enlace impossível seja
    /// *"não dá"* e não lodo.
    pub piso_bps: u32,
    /// Perda, em %, a partir da qual o controlador **desce**.
    ///
    /// **1,0 %.** Na curva medida (A10s → tablet, 2,4 GHz), 1 % separa os degraus em que
    /// `suspeitos` fica na casa das unidades (300-700 kbps: 4, 5, 16) dos em que ele vai à casa
    /// das dezenas e centenas (1500 kbps para cima: 23, 54, 119, 202).
    pub perda_alta_pct: f32,
    /// Perda, em %, abaixo da qual o controlador considera a janela **calma**.
    ///
    /// **0,2 %.** É a ordem de grandeza da melhor janela medida em 2,4 GHz (0,232-0,235 %); em
    /// 5 GHz a perda medida é zero, e lá esta condição está sempre satisfeita.
    pub perda_baixa_pct: f32,
    /// Fator multiplicativo da descida. `0,75` corta um quarto por janela.
    pub fator_desce: f32,
    /// Degrau aditivo da subida, em bps.
    pub degrau_sobe_bps: u32,
    /// **Tempo de calmaria agregada exigido antes de subir**, em ms.
    ///
    /// Não são "N janelas calmas seguidas", e a diferença custou uma corrida inteira. A primeira
    /// versão desta política exigia 4 janelas de 500 ms com perda baixa, e **oscilou em bancada**:
    /// A10s → tablet em 2,4 GHz, o controlador entrou num ciclo-limite
    /// `400 → 650 → 487 → 737 → 553 → 414 → 400` que se repetiu três vezes em 45 s, com 17
    /// descidas e 5 subidas.
    ///
    /// A causa não era o ganho: era a **premissa**. O modelo que aprovou aquela versão dava a
    /// cada janela a perda média do ponto de operação; o enlace real é **em rajadas**. Na corrida
    /// que oscilou, a mediana da perda por janela era **0,00 %** enquanto a sessão inteira perdia
    /// 3,5 % — a maioria das janelas é limpa, e quatro delas seguidas acontecem o tempo todo entre
    /// duas rajadas. "Quatro janelas limpas" não é uma medida de calmaria; é uma medida do
    /// intervalo entre rajadas.
    ///
    /// O que mede calmaria é a perda **agregada** sobre um trecho longo o bastante para conter
    /// uma rajada se houver uma. 8 s é ~4× o intervalo entre rajadas medido naquela corrida.
    pub calmaria_para_subir_ms: u64,
    /// Multiplicador da calmaria exigida, dobrado a cada descida, até este teto.
    ///
    /// O anti-oscilação de segunda ordem: um enlace que já obrigou a descer três vezes tem de
    /// provar muito mais para ganhar banda de volta. Num enlace limpo ele nunca sai de 1, porque
    /// nunca houve descida — o braço de aferição negativo não sente isto.
    pub paciencia_maxima: u32,
    /// Janelas ignoradas depois de cada mudança.
    ///
    /// A janela que atravessa uma troca de bitrate mistura os dois regimes, e decidir sobre ela
    /// é decidir sobre um número que não descreve nenhum dos dois. Uma janela de carência é o
    /// mínimo que faz sentido; é o mesmo motivo pelo qual `aa-escada.py` descarta os primeiros
    /// segundos de cada degrau.
    pub carencia_de_janelas: u32,
    /// Pacotes mínimos numa janela para ela decidir alguma coisa.
    ///
    /// Uma janela com 3 pacotes e 1 perdido diz "33 % de perda" e não sabe nada. Acontece de
    /// verdade: com a tela parada e o GOP longo, um segundo pode não ter quase nada para mandar.
    ///
    /// **20, e o valor está preso entre duas pressões que se opõem.** Ele precisa ser alto para a
    /// razão significar alguma coisa, e baixo para a janela existir lá embaixo: medido em
    /// 31/08/2026, a 350 kbps no fio são **56 pacotes/s**, ou 28 numa janela de 500 ms. Com 30, o
    /// controlador ficava cego exatamente na faixa em que ele mais precisa enxergar, e parava de
    /// decidir a ~530 kbps sem nunca chegar ao piso — pego por teste, e o teste tinha sido
    /// escrito para outra coisa.
    ///
    /// O resíduo é honesto e fica dito: numa janela de 22 pacotes, **um** perdido são 4,5 %, e
    /// isso derruba a taxa. Perto do piso o controlador é grosso. Como o piso é 400 kbps e a
    /// única coisa abaixo dele é dizer ao usuário que o enlace não dá, a grosseria custa pouco.
    pub pacotes_minimos: u64,
}

impl Politica {
    /// A política padrão sobre um teto dado — o bitrate que o produto usaria sem controlador.
    pub fn com_teto(teto_bps: u32) -> Self {
        Self {
            teto_bps,
            piso_bps: PISO_BPS,
            perda_alta_pct: 1.0,
            perda_baixa_pct: 0.2,
            fator_desce: 0.75,
            degrau_sobe_bps: 250_000,
            calmaria_para_subir_ms: 8_000,
            paciencia_maxima: 8,
            carencia_de_janelas: 1,
            pacotes_minimos: 20,
        }
    }
}

/// Por que o controlador mexeu (ou não). Sai no relato de bancada; não decide nada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motivo {
    /// Janela ignorada: carência depois de uma mudança, ou pacotes de menos.
    Ignorada,
    /// Perda acima do limiar alto: desceu.
    Desceu,
    /// Perda acima do limiar alto, mas já estamos no piso.
    NoPiso,
    /// Calmaria persistente: subiu.
    Subiu,
    /// Calmaria persistente, mas já estamos no teto. **O caso do enlace limpo.**
    NoTeto,
    /// Dentro da banda morta, ou ainda contando janelas calmas: nada a fazer.
    Segurou,
    /// **O receptor disse que não está dando conta** e o alvo desceu por causa disso — não por
    /// perda de rede. Ver `Amostra::nao_decodificados`.
    ReceptorAfogado,
    /// O receptor continua afogado **e a descida anterior não aliviou**: ele está preso em
    /// quadros por segundo, não em bits, e descer mais só pioraria a imagem. Segura.
    AfogadoSemAlivio,
}

/// O controlador. Uma instância por sessão de emissão.
#[derive(Debug, Clone)]
pub struct ControleDeTaxa {
    politica: Politica,
    atual_bps: u32,
    /// Acumulado desde a última mudança de taxa: é ele que mede calmaria, e não uma contagem de
    /// janelas. Ver [`Politica::calmaria_para_subir_ms`].
    ms_desde_a_mudanca: u64,
    pacotes_desde_a_mudanca: u64,
    perdidos_desde_a_mudanca: u64,
    /// Multiplicador da calmaria exigida. **Dobra a cada descida e cai pela metade a cada subida
    /// bem-sucedida**, com piso 1.
    ///
    /// A primeira versão só dobrava, e "nunca volta dentro da sessão" era o comentário. Medido em
    /// 31/08/2026: uma sessão de 60 s com sete descidas termina com a paciência no teto (8), o que
    /// exige **64 s de calmaria agregada por degrau**; e como a subida é **aditiva de 250 kbps**,
    /// voltar de 533 kbps ao teto de 4000 são **14 degraus** — cerca de **quinze minutos** de
    /// enlace limpo. Um trecho ruim de meio minuto custava o resto da sessão.
    ///
    /// A decida pela metade é a metade que faltava, e ela não enfraquece a proteção contra
    /// oscilação **onde ela importa**: o primeiro degrau depois de uma rajada continua custando o
    /// mesmo, porque a paciência só cai **depois** de uma subida ter acontecido. Se o enlace ainda
    /// estiver ruim, o evento seguinte é uma descida, que dobra tudo de novo. O ciclo é limitado
    /// por construção.
    ///
    /// Com a decida: 64 + 32 + 16 + 8 + 8… ≈ **3,3 minutos** para o mesmo caminho, contra quinze.
    ///
    /// **A subida continua ADITIVA de propósito**, e isso não é descuido. Descida multiplicativa
    /// com subida aditiva é o desenho clássico de controle de congestão porque é o que converge
    /// sem oscilar; trocar a subida por multiplicativa aceleraria a volta e traria de volta
    /// exatamente a doença que a primeira política teve.
    paciencia: u32,
    carencia: u32,
    janelas: u64,
    descidas: u64,
    subidas: u64,
    /// **Quanto se afoga desde a última descida por afogamento**: soma e número de janelas. A média
    /// é a referência contra a qual se mede se descer aliviou. Ver [`Motivo::AfogadoSemAlivio`].
    ///
    /// Uma média, e não a janela da descida: o afogamento de campo oscila de 6 a 11 por janela sem
    /// mudar de causa (10/09/2026), e contra uma janela só um 7 depois de um 11 passava por alívio.
    afogamento_da_descida: Option<(u64, u64)>,
    /// A janela afogada anterior já parecia alívio? Alívio só conta em **duas seguidas**.
    alivio_na_anterior: bool,
    /// Tempo acumulado sem afogar desde o último afogamento. Passado
    /// [`Politica::calmaria_para_subir_ms`], a memória acima é esquecida.
    ms_sem_afogar: u64,
}

impl ControleDeTaxa {
    /// Nasce **no teto**, que é o valor de produto. Ver o cabeçalho do módulo.
    pub fn novo(politica: Politica) -> Self {
        Self {
            atual_bps: politica.teto_bps,
            politica,
            ms_desde_a_mudanca: 0,
            pacotes_desde_a_mudanca: 0,
            perdidos_desde_a_mudanca: 0,
            paciencia: 1,
            carencia: 0,
            janelas: 0,
            descidas: 0,
            subidas: 0,
            afogamento_da_descida: None,
            alivio_na_anterior: false,
            ms_sem_afogar: 0,
        }
    }

    /// Bitrate em vigor, em bps.
    pub fn atual_bps(&self) -> u32 {
        self.atual_bps
    }

    /// O controlador chegou ao piso e a perda continua alta?
    ///
    /// É a única condição em que a resposta honesta é para o **usuário**, e não para o encoder:
    /// abaixo do piso a imagem não vale a pena, e dizer isso é melhor que entregar lodo. A casca
    /// decide como mostrar; o núcleo só sabe que chegou aqui.
    pub fn no_piso(&self) -> bool {
        self.atual_bps <= self.politica.piso_bps
    }

    /// Quantas janelas foram consideradas, e quantas mudanças saíram. Para o relato.
    pub fn contadores(&self) -> (u64, u64, u64) {
        (self.janelas, self.descidas, self.subidas)
    }

    /// Alimenta uma janela e devolve o bitrate novo, quando houver um.
    ///
    /// `None` quer dizer **não mexa** — que é a resposta na esmagadora maioria das janelas, e é
    /// a resposta certa num enlace limpo em todas elas.
    ///
    /// # Por que `suspeitos` e `idrs_quebrados` não decidem
    ///
    /// Os dois são consequência da perda, não uma segunda causa. Somá-los à decisão seria contar
    /// o mesmo evento duas vezes e descer o dobro do necessário — e a bancada já tem um caso
    /// registrado de dois mecanismos somados consertando o errado. Eles viajam porque são o
    /// número que fecha esta frente, e porque um dia pode aparecer um enlace em que `suspeitos`
    /// sobe sem perda subir. Se isso acontecer, será medida nova, e a política muda com ela.
    pub fn amostra(&mut self, a: Amostra) -> (Option<u32>, Motivo) {
        self.janelas += 1;

        if self.carencia > 0 {
            self.carencia -= 1;
            return (None, Motivo::Ignorada);
        }
        if a.pacotes < self.politica.pacotes_minimos {
            return (None, Motivo::Ignorada);
        }

        // O trecho desde a última mudança. É sobre **ele** que a subida decide; a descida
        // continua decidindo na janela, porque descer depressa é o que protege a imagem.
        self.ms_desde_a_mudanca += a.ms;
        self.pacotes_desde_a_mudanca += a.pacotes;
        self.perdidos_desde_a_mudanca += a.perdidos;

        // **O receptor afogado desce — uma vez, para ver se alivia.**
        //
        // A diferença entre afogamento e perda é de onde vem o dano: a perda é do rádio, o
        // afogamento é do outro computador. A primeira resposta é a mesma — mandar menos —, porque
        // **subir** naquela situação era o que acontecia antes da frente B (§8.63), e é
        // indefensável.
        //
        // **O que não é o mesmo é o que isso resolve.** Um receptor que decodifica em software
        // gasta mais com mais bits, e descer alivia. Um receptor preso em **quadros por segundo**
        // — decodifica 40 de 60 em qualquer bitrate — descarta o mesmo depois da descida, e a
        // primeira versão deste ramo descia de novo a cada janela: 13,5 Mbps ao piso em 25 janelas
        // (`receptor_preso_em_fps_nao_leva_ao_piso` mediu antes de mudar), paciência no teto, e o
        // Android dizendo ao usuário que a *rede* não dava conta.
        //
        // Então cada descida por afogamento passa a acompanhar **quanto se afoga** (a média das
        // janelas afogadas desde ela), e a próxima só acontece se o afogamento cair mais de um
        // quarto abaixo dessa média **em duas janelas seguidas**. Senão, **segura** — sem descer
        // por afogamento, e sem contar a janela como calmaria. As duas exigências vieram do campo:
        // uma janela só de referência deu uma segunda descida por ruído (B1, §8.69).
        //
        // O piso de três quadros existe para não reagir a soluço: um quadro descartado numa janela
        // de 500 ms é a vida normal de qualquer fila.
        if a.nao_decodificados >= 3 {
            self.ms_sem_afogar = 0;
            let aliviou = match self.afogamento_da_descida {
                None => true,
                Some((soma, n)) => {
                    let parece_alivio = a.nao_decodificados * 4 * n < soma * 3;
                    let confirmado = parece_alivio && self.alivio_na_anterior;
                    self.alivio_na_anterior = parece_alivio && !confirmado;
                    if !parece_alivio {
                        // Só a janela que não aliviou entra na média: é ela que descreve o
                        // afogamento de que se quer saber se cedeu.
                        self.afogamento_da_descida = Some((soma + a.nao_decodificados, n + 1));
                    }
                    confirmado
                }
            };
            if aliviou {
                let alvo = ((self.atual_bps as f32) * self.politica.fator_desce) as u32;
                let novo = alvo.max(self.politica.piso_bps);
                self.zerar_trecho();
                self.carencia = self.politica.carencia_de_janelas;
                self.afogamento_da_descida = Some((a.nao_decodificados, 1));
                self.alivio_na_anterior = false;
                if novo < self.atual_bps {
                    self.atual_bps = novo;
                    self.descidas += 1;
                    self.paciencia = (self.paciencia * 2).min(self.politica.paciencia_maxima);
                    return (Some(novo), Motivo::ReceptorAfogado);
                }
                return (None, Motivo::NoPiso);
            }
            // Não aliviou. A perda de rede, se houver, continua mandando descer (o ramo logo
            // abaixo); sem ela, segura — e zera o trecho, porque segurar não é estar bem.
            if a.perda_pct() < self.politica.perda_alta_pct {
                self.zerar_trecho();
                return (None, Motivo::AfogadoSemAlivio);
            }
        } else {
            // Esquecer o afogamento depende de calmaria **dele**, e não de uma janela boa: um
            // afogamento que some por um instante e volta é o mesmo afogamento, e sem isto cada
            // volta dele custaria mais uma descida — a escada que o ramo acima existe para evitar.
            // O prazo é o mesmo que o enlace precisa de calmaria para ganhar um degrau.
            self.ms_sem_afogar += a.ms;
            self.alivio_na_anterior = false;
            if self.ms_sem_afogar >= self.politica.calmaria_para_subir_ms {
                self.afogamento_da_descida = None;
            }
        }

        // **Descer: na janela, sem esperar trecho nenhum.** Uma janela ruim basta.
        if a.perda_pct() >= self.politica.perda_alta_pct {
            let alvo = ((self.atual_bps as f32) * self.politica.fator_desce) as u32;
            let novo = alvo.max(self.politica.piso_bps);
            if novo < self.atual_bps {
                self.atual_bps = novo;
                self.descidas += 1;
                // A paciência dobra: quem já obrigou a descer tem de provar mais para subir.
                self.paciencia = (self.paciencia * 2).min(self.politica.paciencia_maxima);
                self.zerar_trecho();
                self.carencia = self.politica.carencia_de_janelas;
                return (Some(novo), Motivo::Desceu);
            }
            // No piso: o trecho é zerado mesmo assim, senão a rajada que acabou de acontecer
            // contaria como parte de uma calmaria futura.
            self.zerar_trecho();
            return (None, Motivo::NoPiso);
        }

        // **Subir: só sobre o trecho agregado.** Ver [`Politica::calmaria_para_subir_ms`] para
        // por que não é "N janelas limpas seguidas" — essa versão oscilou em bancada.
        let exigido = self.politica.calmaria_para_subir_ms * u64::from(self.paciencia);
        if self.ms_desde_a_mudanca < exigido {
            return (None, Motivo::Segurou);
        }
        let perda_do_trecho = if self.pacotes_desde_a_mudanca == 0 {
            0.0
        } else {
            self.perdidos_desde_a_mudanca as f32 * 100.0 / self.pacotes_desde_a_mudanca as f32
        };
        if perda_do_trecho > self.politica.perda_baixa_pct {
            // Trecho longo e sujo: não sobe, e recomeça a contar. Sem zerar, um trecho de dez
            // minutos com uma rajada no começo acabaria diluindo-a e liberando a subida.
            self.zerar_trecho();
            return (None, Motivo::Segurou);
        }
        if self.atual_bps >= self.politica.teto_bps {
            self.zerar_trecho();
            return (None, Motivo::NoTeto);
        }
        let novo = self
            .atual_bps
            .saturating_add(self.politica.degrau_sobe_bps)
            .min(self.politica.teto_bps);
        self.atual_bps = novo;
        self.subidas += 1;
        // A paciência cai pela metade: quem provou calmaria longa o bastante para subir um degrau
        // não precisa provar o dobro para o seguinte. Ver o campo `paciencia`.
        self.paciencia = (self.paciencia / 2).max(1);
        self.zerar_trecho();
        self.carencia = self.politica.carencia_de_janelas;
        (Some(novo), Motivo::Subiu)
    }

    fn zerar_trecho(&mut self) {
        self.ms_desde_a_mudanca = 0;
        self.pacotes_desde_a_mudanca = 0;
        self.perdidos_desde_a_mudanca = 0;
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    /// Uma janela de 1 s com a perda pedida, e pacotes proporcionais ao bitrate.
    fn janela(bps: u32, perda_pct: f32) -> Amostra {
        // ~110 pacotes/s por Mbps entregue, que é a razão medida em 31/08/2026 (1163 kbps no fio
        // → 167 pacotes/s; 3393 → 413). Não precisa ser exata: o que o teste exercita é a
        // política, e ela só lê a razão perdidos/pacotes.
        let pacotes = (bps as f32 / 1_000_000.0 * 110.0).max(1.0) as u64;
        Amostra {
            ms: 1000,
            pacotes,
            perdidos: (pacotes as f32 * perda_pct / 100.0).round() as u64,
            suspeitos: 0,
            idrs_quebrados: 0,
            nao_decodificados: 0,
        }
    }

    /// **O braço de aferição negativo, e sem ele nada aqui vale.**
    ///
    /// Enlace limpo: perda zero, para sempre. O controlador tem de ficar calado. Em 5 GHz esta
    /// bancada mediu 0 perda em 9 003 pacotes, e o A/B desta frente mediu 0 em 53 001 — baixar a
    /// qualidade ali seria regressão pura.
    ///
    /// O aparelho concorda com este teste: nas duas corridas de 5 GHz com o controlador ligado,
    /// `trocas_de_bitrate = 0` em 181 janelas. Ver `docs/taxa-que-escuta.md` §5.
    #[test]
    fn enlace_limpo_nao_move_nada() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        for _ in 0..600 {
            let (novo, motivo) = c.amostra(Amostra {
                ms: 1000,
                pacotes: 450,
                perdidos: 0,
                suspeitos: 0,
                idrs_quebrados: 0,
                nao_decodificados: 0,
            });
            assert_eq!(novo, None, "mexeu num enlace sem perda nenhuma");
            assert!(matches!(motivo, Motivo::Segurou | Motivo::NoTeto | Motivo::Ignorada));
        }
        assert_eq!(c.atual_bps(), 4_000_000);
        let (_, descidas, subidas) = c.contadores();
        assert_eq!((descidas, subidas), (0, 0));
    }

    /// O mesmo, com a perda que uma sessão limpa de verdade tem: baixa, mas não exatamente zero.
    #[test]
    fn enlace_quase_limpo_tambem_fica_no_teto() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        for _ in 0..300 {
            // 0,1 % — abaixo do limiar baixo, então "calma": o controlador quer subir e não pode.
            let (novo, _) = c.amostra(janela(4_000_000, 0.1));
            assert_eq!(novo, None);
        }
        assert_eq!(c.atual_bps(), 4_000_000);
    }

    /// **Não oscila**: com perda persistente, a taxa converge em vez de bater entre dois valores.
    ///
    /// O enlace aqui é o pior caso para um controlador: a perda **não melhora** por mais que ele
    /// desça. É o caso em que um controlador de um limiar só desceria ao piso e voltaria a subir
    /// assim que a perda passasse do limiar para baixo — e não passa. Aqui ele tem de descer até
    /// o piso e **ficar lá**, sem subir e descer.
    #[test]
    fn perda_persistente_converge_no_piso_e_nao_volta() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        let mut mudancas = 0;
        for _ in 0..400 {
            if c.amostra(janela(c.atual_bps(), 5.0)).0.is_some() {
                mudancas += 1;
            }
        }
        assert!(c.no_piso(), "não chegou ao piso: {} bps", c.atual_bps());
        // De 4 Mbps ao piso de 400 kbps a 0,75 por degrau são 9 descidas. Nada além disso pode
        // ter acontecido: qualquer subida obrigaria a uma décima descida.
        assert_eq!(mudancas, 9, "mudou mais vezes do que a descida geométrica exige");
        let (_, descidas, subidas) = c.contadores();
        assert_eq!((descidas, subidas), (9, 0));
    }

    /// **Não oscila**, o caso interessante: a perda cede quando a taxa cede.
    ///
    /// O enlace é o medido em 31/08/2026 (ver [`modelo_medido`]). O controlador tem de parar num
    /// ponto e **ficar** lá — nem no piso, nem no teto —, e o número de mudanças depois de
    /// assentar tem de ser zero.
    #[test]
    fn enlace_medido_assenta_e_para_de_mexer() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        for _ in 0..40 {
            let bps = c.atual_bps();
            c.amostra(janela(bps, modelo_medido(bps)));
        }
        let assentou_em = c.atual_bps();
        let (_, d0, s0) = c.contadores();
        for _ in 0..400 {
            let bps = c.atual_bps();
            c.amostra(janela(bps, modelo_medido(bps)));
        }
        let (_, d1, s1) = c.contadores();
        assert_eq!(
            c.atual_bps(),
            assentou_em,
            "saiu do ponto de operação depois de assentar",
        );
        assert_eq!((d1 - d0, s1 - s0), (0, 0), "continuou mexendo depois de assentar");
        assert!(!c.no_piso(), "assentou no piso num enlace que não exige isso");
        assert!(assentou_em < 4_000_000, "não desceu num enlace com 2,8 % de perda no teto");
        // **O ponto de operação, fixado.** Contra a curva medida em 31/08/2026, o controlador
        // assenta em ~949 kbps pedidos — a faixa em que a mesma bancada mediu 1,3 % de perda e
        // `suspeitos` na casa das dezenas, contra 2,8-3,3 % e 190-202 no teto de 4000. Este
        // número está travado aqui de propósito: se alguém mexer num parâmetro da política, o
        // teste diz para onde o ponto andou, em vez de deixar a mudança passar calada.
        assert_eq!(assentou_em, 949_218);
    }

    /// Devolve o que a rede fez ao subir de novo: sobe até o teto e não passa dele.
    #[test]
    fn devolve_o_que_tirou_e_para_no_teto() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        for _ in 0..40 {
            c.amostra(janela(c.atual_bps(), 5.0));
        }
        assert!(c.no_piso());
        // A subida custa 8 s de calmaria agregada por degrau, multiplicados pela paciência (que
        // as descidas levaram ao teto de 8): são 64 s por degrau, e 15 degraus até o teto.
        for _ in 0..2000 {
            c.amostra(janela(c.atual_bps(), 0.0));
        }
        assert_eq!(c.atual_bps(), 4_000_000, "não devolveu tudo o que tirou");
        // E não passa: nem um bit acima do que o produto usaria sem controlador.
        for _ in 0..100 {
            assert_eq!(c.amostra(janela(4_000_000, 0.0)).0, None);
        }
        assert_eq!(c.atual_bps(), 4_000_000);
    }

    /// Subir é aditivo e descer é multiplicativo: recuperar custa mais janelas do que recuar.
    #[test]
    fn desce_rapido_e_sobe_devagar() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        let mut janelas_para_descer = 0;
        while !c.no_piso() {
            janelas_para_descer += 1;
            c.amostra(janela(c.atual_bps(), 5.0));
            assert!(janelas_para_descer < 100, "não desceu");
        }
        let mut janelas_para_subir = 0;
        while c.atual_bps() < 4_000_000 {
            janelas_para_subir += 1;
            c.amostra(janela(c.atual_bps(), 0.0));
            assert!(janelas_para_subir < 5000, "não subiu");
        }
        assert!(
            janelas_para_subir > 3 * janelas_para_descer,
            "subida ({janelas_para_subir}) não é bem mais lenta que a descida ({janelas_para_descer})",
        );
    }

    /// Uma janela sem pacotes suficientes não decide nada — nem para cima, nem para baixo.
    #[test]
    fn janela_magra_nao_decide() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        for _ in 0..200 {
            let (novo, motivo) = c.amostra(Amostra {
                ms: 1000,
                pacotes: 3,
                perdidos: 1, // 33 % — e não sabe nada
                suspeitos: 0,
                idrs_quebrados: 0,
                nao_decodificados: 0,
            });
            assert_eq!(novo, None);
            assert_eq!(motivo, Motivo::Ignorada);
        }
        assert_eq!(c.atual_bps(), 4_000_000);
    }

    /// A janela imediatamente depois de uma mudança é ignorada — ela mistura os dois regimes.
    #[test]
    fn carencia_ignora_a_janela_que_atravessa_a_troca() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        assert!(matches!(c.amostra(janela(4_000_000, 5.0)), (Some(_), Motivo::Desceu)));
        // A seguinte tem perda altíssima e mesmo assim não desce de novo.
        assert_eq!(c.amostra(janela(3_000_000, 50.0)), (None, Motivo::Ignorada));
        // A terceira já decide.
        assert!(matches!(c.amostra(janela(3_000_000, 50.0)), (Some(_), Motivo::Desceu)));
    }

    /// Perda dentro da banda morta segura a taxa parada, para sempre.
    #[test]
    fn banda_morta_segura() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        for _ in 0..500 {
            // 0,5 %: acima do baixo (0,2) e abaixo do alto (1,0).
            assert_eq!(c.amostra(janela(4_000_000, 0.5)), (None, Motivo::Segurou));
        }
        assert_eq!(c.atual_bps(), 4_000_000);
    }

    /// O piso é piso: o controlador para nele e diz que parou.
    #[test]
    fn o_piso_e_piso() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        for _ in 0..200 {
            c.amostra(janela(c.atual_bps(), 20.0));
        }
        assert!(c.no_piso());
        assert_eq!(c.atual_bps(), 400_000);
        assert_eq!(c.amostra(janela(400_000, 20.0)), (None, Motivo::NoPiso));
    }

    /// Contador que anda para trás (track recriada) não vira perda negativa nem taxa maior.
    ///
    /// A defesa mora na casca que monta a [`Amostra`] — `JanelaDoEnlace`, no Android —, e este
    /// teste fixa o contrato deste lado: perdidos maior que pacotes é entrada absurda, e a
    /// resposta é descer, nunca subir.
    /// **O caso que a frente B existe para consertar**, e ele é literalmente o de campo: perda de
    /// rede zero, e o receptor descartando quadros porque não decodifica no ritmo. Antes deste
    /// conserto, esta amostra fazia o controlador **subir**.
    #[test]
    fn receptor_afogado_desce_mesmo_com_perda_zero() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        let antes = c.atual_bps();
        let (novo, motivo) = c.amostra(Amostra {
            ms: 500,
            pacotes: 500,
            perdidos: 0,
            suspeitos: 0,
            idrs_quebrados: 0,
            nao_decodificados: 17,
        });
        assert!(matches!(motivo, Motivo::ReceptorAfogado), "veio {motivo:?}");
        assert!(novo.unwrap() < antes, "tinha de descer: {novo:?} contra {antes}");
    }

    /// Uma janela de 500 ms sem perda nenhuma e com `nao` quadros que o receptor não entregou.
    fn afogada(nao: u64) -> Amostra {
        Amostra {
            ms: 500,
            pacotes: 800,
            perdidos: 0,
            suspeitos: 0,
            idrs_quebrados: 0,
            nao_decodificados: nao,
        }
    }

    /// **O receptor preso em fps não pode levar o emissor ao piso.**
    ///
    /// Um receptor que decodifica 40 de 60 quadros por segundo descarta ~10 por janela **em
    /// qualquer bitrate** — baixar bits não baixa quadros. A primeira versão de
    /// `ReceptorAfogado` descia a cada janela afogada, e com carência de uma janela e fator 0,75
    /// isso leva 13,5 Mbps ao piso de 400 kbps em ~12 s, com a paciência no teto: ~8 minutos para
    /// voltar, e o Android dizendo ao usuário que *"a rede não está dando conta"* — a causa
    /// errada. Apontado pela revisão adversarial de 10/09/2026, antes de o gatilho disparar uma
    /// vez em campo.
    ///
    /// O desenho agora: desce **uma** vez para ver se alivia; se a janela afogada seguinte não
    /// aliviou, **segura**.
    #[test]
    fn receptor_preso_em_fps_nao_leva_ao_piso() {
        let teto = 13_500_000;
        let mut c = ControleDeTaxa::novo(Politica::com_teto(teto));
        let mut trajetoria = Vec::new();
        for _ in 0..60 {
            c.amostra(afogada(10));
            trajetoria.push(c.atual_bps());
        }
        assert!(!c.no_piso(), "chegou ao piso: {trajetoria:?}");
        let (_, descidas, _) = c.contadores();
        assert_eq!(descidas, 1, "uma descida para testar se alivia, e não mais: {trajetoria:?}");
        assert_eq!(c.atual_bps(), (teto as f32 * 0.75) as u32);
    }

    /// **O ruído de campo não pode passar por alívio.** A sequência é a de `nao_entregues` que o
    /// S24 registrou em 10/09/2026 com o Dell preso em 40 fps (B1, `docs/bancada.md` §8.69): o
    /// afogamento oscila de 6 a 11 por janela sem mudar de causa. Comparar cada janela com **uma**
    /// janela de referência deu uma segunda descida por causa de um 7 contra um 11.
    #[test]
    fn afogamento_ruidoso_de_campo_nao_passa_por_alivio() {
        let campo: [u64; 38] = [
            11, 10, 9, 10, 10, 9, 7, 10, 8, 8, 10, 9, 10, 7, 9, 9, 7, 9, 11, 9, 9, 6, 9, 9, 9, 8,
            9, 9, 7, 8, 8, 9, 10, 6, 7, 7, 9, 10,
        ];
        let mut c = ControleDeTaxa::novo(Politica::com_teto(13_500_000));
        for nao in campo.iter().cycle().take(200) {
            c.amostra(afogada(*nao));
        }
        let (_, descidas, _) = c.contadores();
        assert_eq!(descidas, 1, "o ruído de 6 a 11 não é alívio");
    }

    /// **Quando descer alivia, continua descendo** — é o receptor que decodifica em software, em
    /// que o custo de decodificar cresce com os bits.
    #[test]
    fn receptor_que_alivia_com_a_descida_continua_descendo_ate_parar_de_afogar() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(13_500_000));
        // O afogamento cai pela metade a cada descida, até sumir.
        let mut nao = 16u64;
        let mut descidas_vistas = 0;
        for _ in 0..40 {
            let (novo, motivo) = c.amostra(afogada(nao));
            if novo.is_some() {
                assert!(matches!(motivo, Motivo::ReceptorAfogado), "veio {motivo:?}");
                descidas_vistas += 1;
                nao /= 2;
            }
        }
        assert_eq!(descidas_vistas, 3, "16 → 8 → 4 → 2: três descidas, e em 2 parou de afogar");
    }

    /// Segurar por afogamento **não** pode esconder perda de rede: se a janela afogada também
    /// perde pacote, a perda manda descer.
    #[test]
    fn segurar_por_afogamento_nao_esconde_perda_de_rede() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(13_500_000));
        c.amostra(afogada(10)); // desce uma vez
        c.amostra(afogada(10)); // carência
        let (_, motivo) = c.amostra(afogada(10));
        assert!(matches!(motivo, Motivo::AfogadoSemAlivio), "veio {motivo:?}");
        let antes = c.atual_bps();
        let (novo, motivo) = c.amostra(Amostra { perdidos: 40, ..afogada(10) }); // 5 %
        assert!(matches!(motivo, Motivo::Desceu), "veio {motivo:?}");
        assert!(novo.unwrap() < antes);
    }

    /// **Um afogamento que some e volta é o mesmo afogamento.** Sem memória entre as voltas, cada
    /// uma custaria mais uma descida — a escada até o piso por outro caminho.
    #[test]
    fn afogamento_intermitente_nao_vira_escada() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(13_500_000));
        for _ in 0..10 {
            // 2 s afogado, 2 s bem, dez vezes.
            for _ in 0..4 {
                c.amostra(afogada(10));
            }
            for _ in 0..4 {
                c.amostra(afogada(0));
            }
        }
        let (_, descidas, _) = c.contadores();
        assert_eq!(descidas, 1);
    }

    /// E ela é esquecida depois de calmaria longa: o receptor pode ter mudado de situação, e o
    /// afogamento novo merece de novo a descida que testa se alivia.
    #[test]
    fn depois_de_calmaria_longa_o_afogamento_novo_desce_de_novo() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(13_500_000));
        c.amostra(afogada(10));
        for _ in 0..40 {
            c.amostra(afogada(0)); // 20 s sem afogar
        }
        let antes = c.atual_bps();
        let (novo, motivo) = c.amostra(afogada(10));
        assert!(matches!(motivo, Motivo::ReceptorAfogado), "veio {motivo:?}");
        assert!(novo.unwrap() < antes);
    }

    /// E a janela afogada **não conta como calmaria**: segurar não é o mesmo que estar bem, e
    /// subir enquanto o receptor descarta quadro seria a doença de §8.63 de volta.
    #[test]
    fn afogado_segurando_nao_sobe() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(13_500_000));
        for _ in 0..200 {
            let (_, motivo) = c.amostra(afogada(10));
            assert!(!matches!(motivo, Motivo::Subiu), "subiu com o receptor afogado");
        }
    }

    /// **Um quadro descartado não é afogamento.** Uma fila que solta um quadro por janela é a
    /// vida normal; reagir a isso seria descer para sempre por ruído.
    #[test]
    fn um_quadro_descartado_nao_derruba_a_taxa() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        let antes = c.atual_bps();
        let (novo, motivo) = c.amostra(Amostra {
            ms: 500,
            pacotes: 500,
            perdidos: 0,
            suspeitos: 0,
            idrs_quebrados: 0,
            nao_decodificados: 1,
        });
        assert!(!matches!(motivo, Motivo::ReceptorAfogado), "veio {motivo:?}");
        assert_eq!(c.atual_bps(), antes);
        assert!(novo.is_none());
    }

    #[test]
    fn amostra_absurda_nao_faz_subir() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        let (novo, motivo) = c.amostra(Amostra {
            ms: 1000,
            pacotes: 100,
            perdidos: 900,
            suspeitos: 0,
            idrs_quebrados: 0,
            nao_decodificados: 0,
        });
        assert!(matches!(motivo, Motivo::Desceu));
        assert!(novo.unwrap() < 4_000_000);
    }

    /// **Quanto tempo custa voltar ao teto depois de um trecho ruim.**
    ///
    /// Este teste existe porque o número era desconhecido até 31/08/2026 e, quando foi medido, era
    /// **quinze minutos**: sete descidas põem a paciência no teto (8), cada degrau exige
    /// `8 × 8 s = 64 s` de calmaria agregada, e a subida é aditiva de 250 kbps — 14 degraus de
    /// 533 kbps até 4 Mbps.
    ///
    /// Com a paciência caindo pela metade a cada subida, o mesmo caminho cabe em minutos. O teste
    /// mede o tempo de recuperação em segundos simulados e falha se ele voltar a passar de cinco
    /// minutos — é um teto, não um alvo, para que uma mudança de política que o piore precise
    /// dizer isso em voz alta.
    #[test]
    fn a_volta_ao_teto_depois_de_um_trecho_ruim_cabe_em_minutos() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        // 1. O trecho ruim: perda alta até o controlador descer sete degraus.
        while c.contadores().1 < 7 {
            let bps = c.atual_bps();
            let pacotes = (bps as f32 / 1_000_000.0 * 110.0 / 2.0).max(1.0) as u64;
            c.amostra(Amostra {
                ms: 500,
                pacotes,
                perdidos: (pacotes as f32 * 0.12) as u64,
                suspeitos: 0,
                idrs_quebrados: 0,
                nao_decodificados: 0,
            });
        }
        let fundo = c.atual_bps();
        assert!(fundo < 700_000, "esperava ter descido bastante, parou em {fundo}");

        // 2. O enlace fica limpo. Quantas janelas de 500 ms até voltar ao teto?
        let mut janelas = 0u64;
        while c.atual_bps() < 4_000_000 && janelas < 4_000 {
            let bps = c.atual_bps();
            let pacotes = (bps as f32 / 1_000_000.0 * 110.0 / 2.0).max(1.0) as u64;
            c.amostra(Amostra {
                ms: 500,
                pacotes,
                perdidos: 0,
                suspeitos: 0,
                idrs_quebrados: 0,
                nao_decodificados: 0,
            });
            janelas += 1;
        }
        let segundos = janelas / 2;
        assert_eq!(c.atual_bps(), 4_000_000, "não voltou ao teto em {segundos} s");
        assert!(
            segundos <= 300,
            "a volta ao teto levou {segundos} s — antes da paciência decrescente eram ~900",
        );
    }

    /// **O teste que a bancada escreveu, e que a versão anterior desta política reprovava.**
    ///
    /// O enlace aqui é **em rajadas**, e é essa a forma real do problema: na corrida de
    /// 31/08/2026 que reprovou a primeira versão, a mediana da perda por janela era **0,00 %**
    /// enquanto a sessão perdia **3,53 %** — uma janela em cada cinco carrega tudo.
    ///
    /// Com a política de "4 janelas limpas seguidas", este enlace produzia o ciclo-limite
    /// `400 → 650 → 487 → 737 → 553 → 414 → 400`, medido em aparelho: 17 descidas e 5 subidas em
    /// 45 s. O modelo estacionário de [`modelo_medido`] **não pegava isso**, porque dava a cada
    /// janela a perda média do ponto — e a média nunca é limpa.
    ///
    /// É o oitavo instrumento desta semana a errar, e o primeiro desta frente. Ele foi pego pelo
    /// aparelho e não pelo teste, o que é exatamente a ordem errada; este teste existe para que a
    /// próxima vez seja a ordem certa.
    #[test]
    fn enlace_em_rajadas_nao_faz_o_controlador_oscilar() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        // Uma rajada a cada cinco janelas de 500 ms — ~2,4 % agregado, como a corrida medida.
        let rajada = |i: usize| if i % 5 == 0 { 12.0 } else { 0.0 };
        let mut trajetoria = Vec::new();
        for i in 0..600 {
            let bps = c.atual_bps();
            let pacotes = (bps as f32 / 1_000_000.0 * 110.0 / 2.0).max(1.0) as u64;
            c.amostra(Amostra {
                ms: 500,
                pacotes,
                perdidos: (pacotes as f32 * rajada(i) / 100.0).round() as u64,
                suspeitos: 0,
                idrs_quebrados: 0,
                nao_decodificados: 0,
            });
            trajetoria.push(c.atual_bps());
        }
        let (_, descidas, subidas) = c.contadores();
        assert!(c.no_piso(), "não convergiu: parou em {} bps", c.atual_bps());
        // Nove descidas levam 4 Mbps ao piso. Qualquer subida obrigaria a uma décima descida, e é
        // exatamente isso que o ciclo-limite fazia: 17 descidas e 5 subidas em 45 s.
        assert_eq!(
            (descidas, subidas),
            (9, 0),
            "oscilou: {descidas} descidas e {subidas} subidas — trajetória {:?}",
            &trajetoria[..40],
        );
        // E fica parado no fim: as últimas 300 janelas não mexeram em nada.
        assert!(trajetoria[300..].windows(2).all(|p| p[0] == p[1]), "mexeu depois de convergir");
    }

    /// O outro lado da mesma moeda: um enlace em rajadas que **para** de ter rajadas devolve a
    /// banda. Se não devolvesse, o controlador seria só um jeito caro de piorar a imagem.
    #[test]
    fn quando_as_rajadas_cessam_a_banda_volta() {
        let mut c = ControleDeTaxa::novo(Politica::com_teto(4_000_000));
        for i in 0..300 {
            let bps = c.atual_bps();
            let pacotes = (bps as f32 / 1_000_000.0 * 55.0).max(1.0) as u64;
            let perda = if i % 5 == 0 { 12.0 } else { 0.0 };
            c.amostra(Amostra {
                ms: 500,
                pacotes,
                perdidos: (pacotes as f32 * perda / 100.0).round() as u64,
                suspeitos: 0,
                idrs_quebrados: 0,
                nao_decodificados: 0,
            });
        }
        assert!(c.no_piso());
        // O rádio limpou. Com a paciência no teto (8), são 64 s de calmaria agregada por degrau.
        for _ in 0..3000 {
            let bps = c.atual_bps();
            let pacotes = (bps as f32 / 1_000_000.0 * 55.0).max(1.0) as u64;
            c.amostra(Amostra { ms: 500, pacotes, perdidos: 0, suspeitos: 0, idrs_quebrados: 0, nao_decodificados: 0 });
        }
        assert_eq!(c.atual_bps(), 4_000_000, "não devolveu a banda num enlace que limpou");
    }

    /// A curva medida em 31/08/2026, A10s → tablet, 2,4 GHz, braço **ascendente** (o que não
    /// carrega a história dos degraus altos): perda em % contra bitrate entregue no fio.
    ///
    /// | fio kbps | 351 | 536 | 786 | 1163 | 1601 | 2336 | 2803 | 3393 |
    /// |---|---|---|---|---|---|---|---|---|
    /// | perda % | 0,235 | 0,232 | 0,939 | 1,341 | 2,167 | 1,548 | 2,662 | 2,830 |
    ///
    /// Interpolação linear entre os pontos, com as pontas planas. **Não é um modelo de rádio** —
    /// é a tabela que a bancada mediu, usada para que o teste exercite a política contra a forma
    /// real do problema, e não contra uma curva inventada que a favoreça.
    fn modelo_medido(bps: u32) -> f32 {
        const PONTOS: [(f32, f32); 8] = [
            (351.0, 0.235),
            (536.0, 0.232),
            (786.0, 0.939),
            (1163.0, 1.341),
            (1601.0, 2.167),
            (2336.0, 1.548),
            (2803.0, 2.662),
            (3393.0, 2.830),
        ];
        // O que o encoder entrega no fio não é o que se pede; medido, a razão fica entre 0,85 e
        // 1,17 e vale ~0,9 na faixa alta. Aproximar por 0,9 mantém o teste na região certa da
        // tabela sem fingir precisão que ela não tem.
        let kbps = bps as f32 / 1000.0 * 0.9;
        if kbps <= PONTOS[0].0 {
            return PONTOS[0].1;
        }
        for par in PONTOS.windows(2) {
            let (x0, y0) = par[0];
            let (x1, y1) = par[1];
            if kbps <= x1 {
                return y0 + (y1 - y0) * (kbps - x0) / (x1 - x0);
            }
        }
        PONTOS[PONTOS.len() - 1].1
    }

    /// A aferição do próprio modelo: ele tem de devolver a tabela nos pontos da tabela.
    ///
    /// *Instrumento não aferido contra caso conhecido não é instrumento* — e um modelo de enlace
    /// usado para julgar um controlador é instrumento. Sete instrumentos deste projeto erraram
    /// nesta semana e todos foram pegos pela própria aferição.
    #[test]
    fn o_modelo_devolve_a_tabela_medida() {
        for (kbps_fio, perda) in [
            (351.0_f32, 0.235_f32),
            (786.0, 0.939),
            (1601.0, 2.167),
            (3393.0, 2.830),
        ] {
            let pedido = (kbps_fio / 0.9 * 1000.0) as u32;
            let saiu = modelo_medido(pedido);
            assert!(
                (saiu - perda).abs() < 0.02,
                "modelo em {kbps_fio} kbps deu {saiu}, a bancada mediu {perda}",
            );
        }
        // E é monotônico onde a tabela é monotônica — a exceção medida em 2336 kbps está na
        // tabela e é deliberada: o teste não a esconde.
        assert!(modelo_medido(400_000) < modelo_medido(4_000_000));
    }
}
