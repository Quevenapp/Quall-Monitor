//! **Qual identidade de monitor cada aparelho usa** quando o emissor atende vários receptores.
//!
//! Porte de `apps/macos/Sources/QuallCaptureKit/TabelaDeIndices.swift`, com as mesmas regras e os
//! mesmos números — é aritmética pura, sem Windows, sem rede e sem disco, para se testar no
//! `cargo test` sem mexer nas preferências de ninguém. Quem grava é o coordenador
//! (`crate::varias`), em `%APPDATA%\Quall\indices-de-monitor.json`.
//!
//! # Por que o índice é do aparelho
//!
//! No Mac, o sistema guarda posição e modo por identidade de monitor: com uma identidade por
//! aparelho ele lembra onde a pessoa pôs **o monitor do tablet** e onde pôs **o do iPhone**. No
//! Windows o monitor virtual ainda não existe (é a frente do driver), e o que ele vai lembrar por
//! identidade **não foi medido** — a regra vem pronta do Mac e fica valendo até a medida dizer o
//! contrário. O que ela garante aqui já vale sem driver nenhum: o mesmo aparelho volta com o mesmo
//! número, e dois monitores vivos nunca dividem um.
//!
//! # As duas regras da revisão de 10/09/2026 (a do Mac)
//!
//! - **Dois monitores vivos nunca dividem índice.** Se o índice de um aparelho estiver em uso por
//!   outra sessão (o mesmo aparelho reconectando antes de a sessão velha sair — que a tabela de
//!   sessões já evita esperando a velha desmontar), sai um índice livre, só para esta vez, sem
//!   gravar.
//! - **A tabela tem teto** ([`TabelaDeIndices::TETO`]). Receptores de sonda nascem com `device_id`
//!   novo a cada corrida; sem teto, cada corrida de bancada deixaria uma identidade lembrada para
//!   sempre. Cheia, sai o aparelho usado há mais tempo que não esteja no ar.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entrada {
    pub indice: usize,
    /// Segundos desde 1970 do último uso.
    pub ultimo_uso: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TabelaDeIndices {
    entradas: BTreeMap<String, Entrada>,
}

impl TabelaDeIndices {
    /// O mesmo do Mac.
    pub const TETO: usize = 32;

    pub fn nova() -> Self {
        Self::default()
    }

    /// Lê o que foi gravado. Arquivo ilegível devolve a tabela vazia: o preço é um aparelho ganhar
    /// outro número uma vez, e não vale derrubar uma sessão por isso.
    pub fn de_json(texto: &str) -> Self {
        serde_json::from_str(texto).unwrap_or_default()
    }

    pub fn para_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn len(&self) -> usize {
        self.entradas.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entradas.is_empty()
    }

    /// O índice gravado deste aparelho, sem mexer em nada.
    pub fn gravado(&self, device_id: &str) -> Option<usize> {
        self.entradas.get(device_id).map(|e| e.indice)
    }

    /// O índice deste aparelho agora. `em_uso`: os índices dos monitores que estão no ar —
    /// **inclusive os que estão saindo**, que ainda não soltaram o monitor.
    pub fn indice(&mut self, device_id: &str, em_uso: &BTreeSet<usize>, agora: f64) -> usize {
        if let Some(e) = self.entradas.get_mut(device_id) {
            if em_uso.contains(&e.indice) {
                // O índice dele está com outro monitor vivo: um livre, só desta vez, sem gravar.
                let ocupados = self.ocupados(em_uso);
                return menor_livre(&ocupados);
            }
            e.ultimo_uso = agora;
            return e.indice;
        }
        if self.entradas.len() >= Self::TETO {
            let velho = self
                .entradas
                .iter()
                .filter(|(_, e)| !em_uso.contains(&e.indice))
                .min_by(|a, b| a.1.ultimo_uso.total_cmp(&b.1.ultimo_uso))
                .map(|(k, _)| k.clone());
            if let Some(velho) = velho {
                self.entradas.remove(&velho);
            }
        }
        let novo = menor_livre(&self.ocupados(em_uso));
        self.entradas.insert(device_id.to_string(), Entrada { indice: novo, ultimo_uso: agora });
        novo
    }

    /// Um índice para quem não disse `device_id`: o menor livre, **sem gravar** — não há como
    /// reconhecer esse aparelho da próxima vez, e gravar seria lembrar para sempre um desconhecido.
    pub fn indice_sem_identidade(&self, em_uso: &BTreeSet<usize>) -> usize {
        menor_livre(&self.ocupados(em_uso))
    }

    fn ocupados(&self, em_uso: &BTreeSet<usize>) -> BTreeSet<usize> {
        let mut o = em_uso.clone();
        o.extend(self.entradas.values().map(|e| e.indice));
        o
    }
}

fn menor_livre(ocupados: &BTreeSet<usize>) -> usize {
    let mut i = 0;
    while ocupados.contains(&i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod testes {
    use super::*;

    fn vazio() -> BTreeSet<usize> {
        BTreeSet::new()
    }

    #[test]
    fn o_mesmo_aparelho_volta_com_o_mesmo_indice() {
        let mut t = TabelaDeIndices::nova();
        assert_eq!(t.indice("tablet", &vazio(), 1.0), 0);
        assert_eq!(t.indice("iphone", &BTreeSet::from([0]), 2.0), 1);
        // O tablet sai e volta: o 0 é dele, mesmo com o iPhone no ar.
        assert_eq!(t.indice("tablet", &BTreeSet::from([1]), 3.0), 0);
        assert_eq!(t.indice("iphone", &BTreeSet::from([0]), 4.0), 1);
    }

    #[test]
    fn dois_monitores_vivos_nunca_dividem_indice_e_o_emprestimo_nao_e_gravado() {
        let mut t = TabelaDeIndices::nova();
        assert_eq!(t.indice("tablet", &vazio(), 1.0), 0);
        // O tablet de novo, com a sessão velha dele ainda de pé no 0: sai outro, sem gravar.
        assert_eq!(t.indice("tablet", &BTreeSet::from([0]), 2.0), 1);
        assert_eq!(t.gravado("tablet"), Some(0));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn aparelho_novo_nao_toma_o_indice_gravado_de_outro_que_nao_esta_no_ar() {
        let mut t = TabelaDeIndices::nova();
        t.indice("a", &vazio(), 1.0);
        t.indice("b", &vazio(), 2.0);
        // "a" (0) e "b" (1) estão fora do ar: "c" ganha o 2, e não o 0 de "a".
        assert_eq!(t.indice("c", &vazio(), 3.0), 2);
    }

    #[test]
    fn cheia_sai_o_usado_ha_mais_tempo_que_nao_esta_no_ar() {
        let mut t = TabelaDeIndices::nova();
        for i in 0..TabelaDeIndices::TETO {
            let n = t.indice(&format!("sonda-{i}"), &vazio(), i as f64 + 10.0);
            assert_eq!(n, i);
        }
        // A mais velha (sonda-0, índice 0) está no ar: quem sai é a sonda-1.
        let n = t.indice("novo", &BTreeSet::from([0]), 1000.0);
        assert_eq!(t.len(), TabelaDeIndices::TETO);
        assert_eq!(t.gravado("sonda-1"), None);
        assert_eq!(t.gravado("sonda-0"), Some(0));
        assert_eq!(n, 1, "o novo herda o menor livre, que era o da que saiu");
    }

    #[test]
    fn sem_identidade_nao_grava() {
        let mut t = TabelaDeIndices::nova();
        t.indice("tablet", &vazio(), 1.0);
        assert_eq!(t.indice_sem_identidade(&BTreeSet::from([1])), 2);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn ida_e_volta_pelo_json_e_arquivo_ilegivel_vira_tabela_vazia() {
        let mut t = TabelaDeIndices::nova();
        t.indice("tablet", &vazio(), 1.5);
        t.indice("iphone", &BTreeSet::from([0]), 2.5);
        let lida = TabelaDeIndices::de_json(&t.para_json());
        assert_eq!(lida, t);
        assert!(TabelaDeIndices::de_json("{ isto não é json").is_empty());
        assert!(TabelaDeIndices::de_json("").is_empty());
    }
}
