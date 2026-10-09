#import "QuallMonitorVirtual.h"

// As classes privadas são descritas por **protocolos**, e não por `@interface` com o nome delas: um
// protocolo não gera referência a classe nenhuma, e o compilador só precisa dele para conhecer os
// seletores. Os nomes e tipos saíram de `class_copyPropertyList`/`class_copyMethodList` no macOS
// 26.6.2 — ver `docs/tela-estendida.md`.

@protocol QMVDescritor <NSObject>
@property (nonatomic, strong) NSString *name;
@property (nonatomic) unsigned int maxPixelsWide;
@property (nonatomic) unsigned int maxPixelsHigh;
@property (nonatomic) unsigned int vendorID;
@property (nonatomic) unsigned int productID;
@property (nonatomic) unsigned int serialNum;
@property (nonatomic) CGSize sizeInMillimeters;
@property (nonatomic, strong) dispatch_queue_t queue;
@property (nonatomic, copy) void (^terminationHandler)(id, id);
@end

@protocol QMVModo <NSObject>
- (instancetype)initWithWidth:(unsigned int)width height:(unsigned int)height refreshRate:(double)refreshRate;
@end

@protocol QMVAjustes <NSObject>
@property (nonatomic, strong) NSArray *modes;
@property (nonatomic) unsigned int hiDPI;
@end

@protocol QMVMonitor <NSObject>
- (instancetype)initWithDescriptor:(id)descriptor;
- (BOOL)applySettings:(id)settings;
@property (nonatomic, readonly) CGDirectDisplayID displayID;
@end

@implementation QuallMonitorVirtualObjC {
    id<QMVMonitor> _monitor;
    dispatch_queue_t _fila;
    NSLock *_trava;
}

+ (BOOL)disponivel {
    return NSClassFromString(@"CGVirtualDisplay") != nil
        && NSClassFromString(@"CGVirtualDisplayDescriptor") != nil
        && NSClassFromString(@"CGVirtualDisplaySettings") != nil
        && NSClassFromString(@"CGVirtualDisplayMode") != nil;
}

- (nullable instancetype)initWithNome:(NSString *)nome
                        larguraPixels:(uint32_t)largura
                         alturaPixels:(uint32_t)altura
                                hertz:(double)hertz
                           milimetros:(CGSize)milimetros
                               vendor:(uint32_t)vendor
                              produto:(uint32_t)produto
                                serie:(uint32_t)serie
                           aoTerminar:(nullable void (^)(void))aoTerminar {
    if (![QuallMonitorVirtualObjC disponivel] || largura == 0 || altura == 0 || largura % 2 || altura % 2) {
        return nil;
    }
    self = [super init];
    if (!self) {
        return nil;
    }
    _trava = [[NSLock alloc] init];
    _fila = dispatch_queue_create("quall.monitor-virtual", DISPATCH_QUEUE_SERIAL);

    id<QMVDescritor> descritor = [[NSClassFromString(@"CGVirtualDisplayDescriptor") alloc] init];
    if (!descritor) {
        return nil;
    }
    descritor.name = nome;
    // **O máximo de pixels é o painel; o modo declarado é o painel em pontos** (ver abaixo).
    descritor.maxPixelsWide = largura;
    descritor.maxPixelsHigh = altura;
    descritor.sizeInMillimeters = milimetros;
    // Vendor, produto e série **fixos**: é por eles que o macOS lembra onde a pessoa pôs o monitor
    // em Ajustes > Monitores, e é por eles que o catálogo reconhece o monitor como nosso.
    descritor.vendorID = vendor;
    descritor.productID = produto;
    descritor.serialNum = serie;
    descritor.queue = _fila;
    if (aoTerminar) {
        void (^bloco)(void) = [aoTerminar copy];
        descritor.terminationHandler = ^(id a, id b) {
            (void)a;
            (void)b;
            bloco();
        };
    }

    id<QMVMonitor> monitor =
        [(id<QMVMonitor>)[NSClassFromString(@"CGVirtualDisplay") alloc] initWithDescriptor:descritor];
    if (!monitor || monitor.displayID == kCGNullDirectDisplay) {
        return nil;
    }

    // **O modo vai em pontos: a metade do painel.** Com `hiDPI = 1`, o macOS lê o modo declarado
    // como pontos e o põe como o 2x nativo sobre os pixels do máximo — e oferece também o 1x nos
    // mesmos pixels. Medido em 11/09 (`docs/tela-estendida.md`, "A regra do 2x"): declarado em
    // pixels, o 2x do painel só existia quando o macOS o gerava por conta própria (1920 × 1200
    // sim; 1920 × 1332, 1520 × 720, 3120 × 1440 e 2436 × 1124 não), e o monitor nascia num 1x com
    // um quarto dos pixels. A escala de nascença continua saindo do tamanho físico.
    id<QMVAjustes> ajustes = [[NSClassFromString(@"CGVirtualDisplaySettings") alloc] init];
    id<QMVModo> modo =
        [(id<QMVModo>)[NSClassFromString(@"CGVirtualDisplayMode") alloc] initWithWidth:largura / 2
                                                                                height:altura / 2
                                                                           refreshRate:hertz];
    if (!ajustes || !modo) {
        return nil;
    }
    ajustes.hiDPI = 1;
    ajustes.modes = @[ modo ];
    if (![monitor applySettings:ajustes]) {
        // `monitor` sai de escopo aqui e o monitor some com ele.
        return nil;
    }

    _monitor = monitor;
    _displayID = monitor.displayID;
    return self;
}

- (void)soltar {
    [_trava lock];
    id<QMVMonitor> indo = _monitor;
    _monitor = nil;
    [_trava unlock];
    // Soltar fora da trava: o `dealloc` do CoreGraphics conversa com o WindowServer.
    indo = nil;
}

- (void)dealloc {
    _monitor = nil;
}

@end
