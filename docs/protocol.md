# Compatibilidade com Quall Studio

O Quall Monitor 0.1.1 usa o protocolo **3** do Quall Studio 1.0.0: WebSocket em
`/quall/v3`, pareamento OPAQUE-3DH e sinalização autenticada e cifrada. O port
preserva o núcleo público de [Quevenapp/Quall, revisão
5842bbc5d52c3be91d3b094fab10d81c0fddf842](https://github.com/Quevenapp/Quall/tree/5842bbc5d52c3be91d3b094fab10d81c0fddf842).
As assinaturas C usadas pelas plataformas foram mantidas. O transporte de mídia
WebRTC/DTLS-SRTP e suas dependências nativas permanecem os existentes.

O Monitor 0.1.0 foi criado com o núcleo anterior: protocolo 2 e rota `/quall/v1`.
Um receptor Studio que pede `/quall/v3` recebe HTTP 404 desse Monitor antes de
qualquer tentativa de PIN. A diferença foi reproduzida no host 0.1.0 e confirmada
nas bibliotecas nativas dos APKs Studio 1.0.0 instalados. A atualização porta o
protocolo completo; não aceita rotas ou pareamento legados como fallback.

No primeiro pareamento, digite o PIN da espera atual. Nas próximas conexões,
deixe o PIN vazio para retomar um vínculo v3 salvo. Registros antigos são
preservados, mas exigem um novo PIN. A identidade e o nome reais do par só são
enviados depois da autenticação. A descoberta mostra um rótulo efêmero `Quall …`,
que serve para escolher a conexão e não para identificar um pareamento salvo.

Monitor e Studio têm identidades locais e portas separadas. O Monitor prefere
7878; o Studio usa 7877 como padrão de vídeo. A porta efetiva é a mostrada na
espera, pois outra sessão pode ocupar a preferida. Use sempre o endereço completo
com essa porta. Uma espera é renovada a cada conexão e as plataformas mantêm até
oito sessões de Monitor ativas, cada uma com seu próprio transporte e monitor.

Os testes do núcleo exercitam rejeição de PIN, adulteração e replay do canal
cifrado, além de oito conexões de Monitor e uma de Studio simultâneas com retomada
por vínculos salvos. São testes de protocolo e transporte em loopback; a
validação de monitores e captura nos aparelhos é registrada separadamente em
[validation.md](validation.md).

O fonte portado mantém MPL-2.0. As novas dependências e seus avisos integrais
acompanham [THIRD_PARTY_NOTICES.txt](../THIRD_PARTY_NOTICES.txt).
