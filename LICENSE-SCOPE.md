# Licenciamento do Quall Monitor

O código próprio autorizado do Quall Monitor é disponibilizado sob **Mozilla Public License 2.0 (MPL-2.0)**, com texto integral em [LICENSE](LICENSE). Esta é a mesma licença aplicada ao código próprio do Quall Studio. O aviso de incompatibilidade do Exhibit B não é aplicado a este projeto.

O escopo próprio abrange os arquivos dos apps macOS e Windows, `crates/quall-core`, `crates/quall-ffi`, `crates/quall-rtc`, `crates/quall-opus` (exceto sua dependência vendorizada), a interface compartilhada em `integrations/camera-windows/fonte`, ferramentas, configuração de build/CI e documentação próprios. O código de interface da câmera é uma dependência técnica herdada; a casca Monitor não oferece câmera virtual. Não há plugin OBS neste repositório.

Veneri & Quellis Ltda. é a responsável pela distribuição. Isto não declara cessão de direitos de todos os autores. Os avisos dos titulares permanecem preservados. O recorte e a revisão de origem estão em [SOURCE.md](SOURCE.md).

## Componentes terceiros

A MPL própria não relicencia `vendor/**`, `crates/quall-opus/vendor/**`, dependências de gerenciadores de pacotes, o driver SudoVDA, textos legais e outros materiais de terceiros. Eles conservam seus termos e atribuições. Consulte [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt), os arquivos de licença nos fontes vendorizados, [avisos do SudoVDA](docs/SudoVDA-NOTICES.txt) e [avisos do runtime WiX](docs/WiX-NOTICES.txt).

O SudoVDA incorporado ao instalador Windows tem termos próprios: alterações do SudoMaker sob as alternativas declaradas pelo autor e base Microsoft sob Microsoft Public License (MS-PL). Não é concedida uma nova licença sobre seus binários por este documento. A origem e os limites da verificação estão em [docs/sudovda.md](docs/sudovda.md).

A licença de software não concede direitos sobre marcas Quall/Quéven, imagens ou conteúdo de usuários. Este recorte exclui registros privados de bancada, arquivos de assinatura local, histórico Git do Studio, aplicativos móveis, plugin OBS e materiais sem autorização específica identificada no escopo do Studio.

## Distribuição

Os fontes cobertos, suas modificações e os patches devem ser disponibilizados aos destinatários dos executáveis. O repositório público correspondente é [Quevenapp/Quall-Monitor](https://github.com/Quevenapp/Quall-Monitor). Cada pacote deve informar sua versão e revisão Git; a oferta de fonte deve apontar para essa revisão exata. Os scripts de empacotamento incluem LICENSE, LICENSE-SCOPE.md, NOTICE.txt e os avisos de terceiros. O download do produto é distribuído pelo site oficial Quéven, sem publicação em lojas.
