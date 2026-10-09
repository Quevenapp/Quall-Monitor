# SudoVDA 1.10.9.289

O Quall Monitor incorpora quatro arquivos de terceiros, sem alterações, para Windows x64. A conferência de 09/10/2026 confirmou byte-identidade com [Nonary/Vibepollo](https://github.com/Nonary/Vibepollo/tree/8bf0ef7d3dbc0402e553deb93bb52e50447c4225/src_assets/windows/drivers/sudovda) no commit `8bf0ef7d3dbc0402e553deb93bb52e50447c4225`.

| Arquivo | Bytes | SHA-256 |
|---|---:|---|
| SudoVDA.cat | 2425 | `2f9189de5604bec9d86f51640cc540639e394d9ad0f8e689129375e95f2d22f8` |
| SudoVDA.cer | 772 | `6accdcd519f6179d967db4eaa20ecf25a732ba30e87f4cffebc768b2c13c9007` |
| SudoVDA.dll | 83216 | `47ee263cb5de9382c6630a2d7f3dafec4a49419f953beec869ca5dd0c460ff63` |
| SudoVDA.inf | 3644 | `ad69ac682756f0cf339b081fac7e6e8159fdf2ca01ca69df8945c7246c286925` |

O [README oficial](https://github.com/SudoMaker/SudoVDA/blob/a4b09fa2aa731a964d0cb5d139cb1e6240e4da12/README.md) oferece MIT/CC0/domínio público para as alterações do SudoMaker. A base Microsoft preserva copyright e está sob [MS-PL](https://github.com/microsoft/Windows-driver-samples/blob/main/LICENSE), que permite redistribuição binária com preservação de avisos e termos compatíveis. O driver não recebe a MPL do código próprio do Monitor. Os textos completos estão em [SudoVDA-NOTICES.txt](SudoVDA-NOTICES.txt).

A árvore de referência é `SudoMaker/SudoVDA` no commit `a4b09fa2aa731a964d0cb5d139cb1e6240e4da12`. Não foi comprovada a receita que gerou esta DLL. O EDID de 256 bytes de `edid.h` nessa árvore aparece na DLL em offset 53392. O upstream agradece AKATrevorJay/edid-generator; o gerador usa GPL, mas a [licença do gerador não se aplica automaticamente ao output](https://www.gnu.org/licenses/gpl-faq.en.html#GPLOutput). Os direitos específicos do EDID não estão certificados por esta conferência.

O certificado público identifica sujeito e emissor `CN=sudovda@su.mk`, com validade de 13/07/2025 a 13/07/2030. Isso não comprova confiança do Windows nem instalação em máquina limpa. A [Microsoft limita a automação de confiança de certificados de teste a sistemas internos](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/trusted-publishers-certificate-store). O instalador verifica o pacote e deve informar falhas sem alterar silenciosamente Root/TrustedPublisher ou reduzir a política de assinatura.

Nenhum termo geral proibindo redistribuição foi identificado nas licenças consultadas. Essa permissão é distinta das limitações de procedência e assinatura acima. Não houve instalação, importação de certificado ou alteração de segurança durante esta preparação.
