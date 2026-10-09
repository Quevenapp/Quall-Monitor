#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>

NS_ASSUME_NONNULL_BEGIN

/// Embrulho fino da API **privada** `CGVirtualDisplay` do CoreGraphics — o monitor que não existe.
///
/// # Por que Objective-C, e por que `NSClassFromString`
///
/// As quatro classes (`CGVirtualDisplay`, `…Descriptor`, `…Settings`, `…Mode`) não têm cabeçalho
/// público. Declará-las para o Swift e instanciá-las pelo nome da classe faria o binário carregar
/// uma **referência de símbolo** a elas: no dia em que uma atualização do macOS as tirar, o `dyld`
/// recusaria abrir o Quall inteiro — inclusive para quem nunca tocou em "Tela estendida". Buscadas
/// por `NSClassFromString`, a ausência vira um `nil` aqui e uma linha a menos no seletor.
///
/// Os seletores usados foram lidos do runtime do macOS 26.6.2 (MacBook Air M4) antes de escrever
/// esta linha — ver `docs/tela-estendida.md`.
///
/// # O que este tipo **não** faz
///
/// Não escolhe o modo de exibição: sobe o monitor com `hiDPI = 1`, o máximo igual aos pixels do
/// painel e um único modo declarado **em pontos** (a metade) — o que faz o 2x do painel existir em
/// qualquer formato (medido em 11/09). Escolher 1x ou 2x, esperar o monitor ficar online e conferir
/// o que o WindowServer aceitou é de `MonitorVirtual.swift`.
@interface QuallMonitorVirtualObjC : NSObject

/// As quatro classes privadas existem neste macOS.
+ (BOOL)disponivel;

/// Cria o monitor. Ele passa a existir para o sistema **enquanto este objeto viver** — soltá-lo, ou
/// o processo morrer, tira o monitor.
///
/// `largura` e `altura` são os pixels do painel, e têm de ser pares (o modo declarado é a metade).
///
/// Devolve `nil` se a API não existir, se os pixels forem ímpares, se `initWithDescriptor:` recusar,
/// ou se `applySettings:` recusar o modo. Em nenhum caso fica monitor para trás.
///
/// `aoTerminar` é chamado se o **sistema** encerrar o monitor por conta própria; roda numa fila
/// privada deste objeto.
- (nullable instancetype)initWithNome:(NSString *)nome
                        larguraPixels:(uint32_t)largura
                         alturaPixels:(uint32_t)altura
                                hertz:(double)hertz
                           milimetros:(CGSize)milimetros
                               vendor:(uint32_t)vendor
                              produto:(uint32_t)produto
                                serie:(uint32_t)serie
                           aoTerminar:(nullable void (^)(void))aoTerminar NS_DESIGNATED_INITIALIZER;

- (instancetype)init NS_UNAVAILABLE;
+ (instancetype)new NS_UNAVAILABLE;

/// O id do monitor para o CoreGraphics e o ScreenCaptureKit. Continua lendo o valor antigo depois
/// de `soltar`, para o registro poder dizer qual monitor foi embora.
@property (nonatomic, readonly) CGDirectDisplayID displayID;

/// Tira o monitor agora. Idempotente e seguro de qualquer thread.
- (void)soltar;

@end

NS_ASSUME_NONNULL_END
