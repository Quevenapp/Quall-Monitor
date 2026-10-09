# Origem dos fontes

O Quall Monitor foi separado do checkout Quall Studio em 09/10/2026, a partir da revisão Git `93b45a82e0eecef42c6f5862210a98ec3778bb92` do projeto Quall (`brunovnq/quall`, produto público em `Quevenapp/Quall`). O repositório Monitor começa com um snapshot selecionado; não importa o histórico Git nem arquivos locais do Studio.

A seleção inclui o núcleo Rust e sua fronteira C, libopus, o fork necessário de datachannel-sys/libdatachannel, captura e recepção desktop, a interface Windows compartilhada e a casca Monitor. Foram excluídos apps Android/iOS, plugin OBS, ferramentas de bancada externa, dados pessoais, certificados de assinatura do produto, credenciais e ícones cuja autorização específica não constava do escopo de licença. O certificado público do pacote SudoVDA é parte do driver, não uma chave privada do produto.

A revisão 0.1.1 atualiza os módulos de protocolo, pareamento, descoberta, sessão e sinalização do núcleo a partir do snapshot público Quall Studio `5842bbc5d52c3be91d3b094fab10d81c0fddf842`. Essa base contém o protocolo v3 usado pelos receptores Studio 1.0.0: rota `/quall/v3`, OPAQUE-3DH e canal de sinalização cifrado. O 0.1.0 carregava a rota antiga `/quall/v1`, incompatível com esses receptores. A atualização mantém o protocolo e suas proteções, sem criar um alias para a negociação antiga.

O ícone do Monitor é um desenho próprio de monitor com a geometria da marca Quall na tela. Seu fonte vetorial está em `assets/QuallMonitor.svg`; `tools/generate-icons.py` reproduz os recursos ICNS e ICO usados pelos aplicativos.

Arquivos próprios mantêm MPL-2.0. Os terceiros mantêm suas licenças. O inventário conservador em THIRD_PARTY_NOTICES.txt foi herdado do Studio e inclui avisos de componentes que não necessariamente participam de cada binário do Monitor; isso não adiciona dependências ao build.

O monitor virtual macOS usa o helper por sessão e a API CGVirtualDisplay disponível no sistema. O Windows usa o pacote SudoVDA identificado em docs/sudovda.md. A casca preserva o protocolo Quall para conectar receptores compatíveis.
