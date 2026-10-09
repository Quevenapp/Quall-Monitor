# Quall Monitor para macOS

Casca independente do Quall Studio, com **Estender tela** e **Exibir**, para download direto no
site oficial. Requer macOS 13 ou posterior. Interface inicial em português. Não usa câmera,
microfone, câmera virtual, OBS, teleprompter ou publicação na Mac App Store.

## Compilar e empacotar

Na raiz do repositório, com Rust, CMake e o Xcode selecionado:

```sh
apps/macos/Empacotar/empacotar.sh
```

Gera `dist/macos/Quall Monitor.app`, `Quall-Monitor-0.1.0-macos-arm64.zip` (ou `x64`) e seu SHA-256. O pipeline
compila o núcleo deste repositório, os dois executáveis Swift e assina localmente com ad-hoc.
Não instala nem publica. O helper `quall-monitor-display` vive dentro do app.

`QUALL_MONITOR_ARQUITETURAS=universal` constrói arm64 + x86_64; exige os dois targets Rust
instalados. `QUALL_MONITOR_IDENTIDADE='Developer ID Application: ...'` usa uma identidade já
existente no Keychain, com hardened runtime. A assinatura ad-hoc serve à validação local;
download público precisa do Developer ID e da notarização do titular. O script não obtém
credenciais nem notariza automaticamente.

Depois de empacotar com Developer ID, use somente um perfil de notarização já salvo no Keychain:

```sh
QUALL_MONITOR_PERFIL_NOTARIZACAO='nome-do-perfil-salvo' apps/macos/Empacotar/notarizar.sh
```

O script também aceita o caminho de um `.app` como primeiro argumento. Ele verifica assinatura,
hardened runtime e timestamp do app e helper antes de enviar; aguarda o resultado JSON e só
prossegue com `Accepted`. Anexa e valida o ticket Apple, e recria o ZIP versionado e o SHA-256
com o app já notarizado. O relato JSON fica ao lado do download. Não cria perfis nem obtém
credenciais; uma rejeição ou falha interrompe o pipeline sem substituir o ZIP final.

`QUALL_MONITOR_NUCLEO_PRONTO=sim` reaproveita somente `target/release/libquall.a` já construído
neste repositório. `QUALL_MONITOR_JOBS`, `QUALL_MONITOR_VERSAO` e `QUALL_MONITOR_BUILD` são opções
de empacotamento. Para desenvolvimento Swift, `QUALL_MONITOR_LIBQUALL` aceita um caminho explícito
de núcleo compatível; o empacotador sempre substitui esse valor pelo núcleo do próprio Monitor.

## Usar

1. Abra `Quall Monitor.app` pelo Finder. No computador que terá mais uma tela, escolha **Estender
   tela** e **Iniciar tela estendida**. Autorize Gravação da Tela quando o macOS pedir.
2. No outro computador, escolha **Exibir**, selecione o aparelho descoberto ou digite o endereço
   completo `IP:porta`, e entre com o PIN de seis dígitos. Receptores Quall Studio compatíveis
   também podem receber a tela.
3. A conexão cria um monitor virtual no formato informado pelo receptor; sem essa informação,
   usa 1920 × 1200. Organize-o em Ajustes do Sistema › Monitores. **Parar**, desconexão ou saída
   remove o monitor. Tela cheia do receptor fica no botão ou em ⌃⌘F.

Esta primeira casca atende **um receptor por sessão**. A rede é local, com `_quall._tcp` e PIN;
a porta preferida é 7878 e passa para uma efêmera se estiver ocupada. Digite a porta indicada
no emissor; um endereço sem porta é recusado para evitar ambiguidade com o Studio (7877).
O Monitor receptor aceita apenas uma track de tela, ignora som e rejeita uma track de câmera.

## Coexistência e implementação

O bundle `br.com.queven.quall.monitor`, os dados em `Library/Application Support/Quall Monitor`,
os registros em `Library/Logs/Quall Monitor` e as preferências são próprios. O UUID é persistido
com prefixo `monitor-mac-`; o anúncio usa `Nome do Mac · Quall Monitor`. O monitor virtual usa
produto `0x0002`, separado do Studio legado (`0x0001`). Os dois aplicativos podem permanecer
abertos; parar o Monitor só encerra a própria sessão/helper.

O monitor usa a API privada `CGVirtualDisplay`, procurada dinamicamente. O app informa quando
ela está ausente. Isso exige validação em cada macOS suportado e é uma razão para este canal
direto. Cada sessão possui um processo helper; fechar o stdin, sair ou morrer remove o monitor.
Captura e codificação vêm do Quall (ScreenCaptureKit → VideoToolbox → núcleo Rust), com 30 fps
padrão, monitor de 60 Hz, IDR sob demanda, repetição de tela parada e espaçamento de saída.

A thread dedicada da sessão é a única que bombeia eventos e fecha a rede. No receptor, perdas
pedem IDR e retêm quadros dependentes até a recuperação; silêncio de 15 segundos encerra a sessão.
O fim desregistra callbacks com barreira, libera tracks e fecha a sessão. O decoder só é fechado
explicitamente após confirmação da barreira; falha mantém a caixa de callback e seu decoder vivos.
Não há gravação de imagem em disco. `QUALL_MONITOR_DADOS=/caminho/absoluto` isola dados de bancada.

## Verificar

```sh
cd apps/macos
xcrun swift test --jobs 2
```

Os testes de modo/escala/identidade do monitor não criam displays nem capturam tela. Os testes
do decoder usam H.264 e pixels sintéticos. Build e testes locais não comprovam conexão física,
Wi-Fi, fluidez percebida, Intel, API privada em macOS antigos, permissão TCC, assinatura Developer
ID ou notarização. Essas provas precisam ser realizadas nos aparelhos antes de distribuir.
