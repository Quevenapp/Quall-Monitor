# SudoVDA 1.10.9.289 — o driver da tela estendida no Windows

Estes quatro arquivos são o **SudoVDA**, o driver de monitor virtual (UMDF/IddCx) do **SudoMaker**
(<https://github.com/SudoMaker/SudoVDA>). **Não são nossos**: o Quall os leva dentro do
`quall-app.exe` (`include_bytes!`, `src/driver_da_tela_estendida.rs`) e só os instala quando a
pessoa clica em "Instalar o driver da tela estendida" e aceita a caixa que explica o que vai
acontecer (decisão do Bruno, 02/10/2026; `docs/monitor-virtual-windows.md` §15).

## Licença

- **SudoVDA**: o autor declara *"MIT and CC0 or Public Domain [...] choose the least restrictive"*
  (README do repositório; o repositório não tem arquivo `LICENSE`). Usamos sob a MIT.
- O SudoVDA é derivado do exemplo `IndirectDisplay` da Microsoft (`Windows-driver-samples`), sob a
  **Microsoft Public License (MS-PL)**. A MS-PL pede que os avisos de direitos autorais, patentes,
  marcas e atribuição sejam mantidos, e que o binário só seja distribuído sob licença compatível com
  ela. Este arquivo é esse aviso: o `SudoVDA.dll` é distribuído aqui sem alteração.

Texto da MIT (para o SudoVDA, © SudoMaker):

> Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
> associated documentation files (the "Software"), to deal in the Software without restriction,
> including without limitation the rights to use, copy, modify, merge, publish, distribute,
> sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions: The above copyright notice and this
> permission notice shall be included in all copies or substantial portions of the Software.
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT
> NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
> NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
> DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT
> OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

O **nefcon** (MIT, Benjamin Höglinger-Stelzer) **não** vai junto: o Quall cria o nó e instala o
driver pela SetupAPI do próprio Windows (`docs/monitor-virtual-windows.md` §15).

## Procedência e conferência

Os quatro vêm do repositório do Vibepollo, `src_assets/windows/drivers/sudovda/` no commit
`8bf0ef7d3dbc0402e553deb93bb52e50447c4225` (o `.inf`, o `.cat` e o `.cer` são os mesmos blobs do
Apollo). Foram **trazidos do Dell da bancada** em 02/10/2026 (`C:\Users\bruno\monitor-virtual`, a
pasta do roteiro do §9, baixada pelo Bruno em 14/09) — nada foi baixado da internet para isto.

| arquivo | bytes | SHA-256 | conferido contra |
|---|---:|---|---|
| `SudoVDA.cat` | 2425 | `2F9189DE5604BEC9D86F51640CC540639E394D9AD0F8E689129375E95F2D22F8` | a cópia no DriverStore do Dell (`sudovda.inf_amd64_b30b37ad037ba94a`), aceita pelo PnP em 14/09; Authenticode `Valid`, `CN=sudovda@su.mk` |
| `SudoVDA.cer` | 772 | `6ACCDCD519F6179D967DB4EAA20ECF25A732BA30E87F4CFFEBC768B2C13C9007` | §9; impressão SHA-1 `3C918FC73525AD8B1521B6DB26B71F694277CC49` |
| `SudoVDA.dll` | 83216 | `47EE263CB5DE9382C6630A2D7F3DAFEC4A49419F953BEEC869CA5DD0C460FF63` | a cópia no DriverStore do Dell; Authenticode `Valid` pelo catálogo, `CN=sudovda@su.mk` |
| `SudoVDA.inf` | 3644 | `AD69AC682756F0CF339B081FAC7E6E8159FDF2CA01CA69DF8945C7246C286925` | §9 (`DriverVer = 07/14/2025,1.10.9.289`) |

Os mesmos valores estão em `src/regras_do_driver.rs`, e um teste confere estes arquivos contra eles.
O `.gitattributes` desta pasta impede o git de mexer no fim de linha (o `.inf` é UTF-16LE com CRLF).
