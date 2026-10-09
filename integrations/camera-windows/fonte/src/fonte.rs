//! A fonte de mídia (`IMFMediaSourceEx` + `IMFGetService` + `IKsControl`).
//!
//! O contrato está em *Frame Server Custom Media Source*: essas três, mais `IMFMediaSource` e
//! `IMFMediaEventGenerator` que a primeira herda, são **obrigatórias**. `IMFGetService` pode não
//! oferecer serviço nenhum, mas precisa existir — uma fonte sem ela é rejeitada pelo pipeline.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use windows::core::{implement, IUnknownImpl, Interface, Ref, Result, GUID, HRESULT, PWSTR};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::Foundation::{E_INVALIDARG, E_NOTIMPL, S_OK};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSIDENTIFIER};
use windows::Win32::Media::MediaFoundation::*;

use crate::diga;
use crate::fluxo::{EstadoFluxo, Fluxo};
use crate::quadros::{Provedor, ALTURA, BYTES_NV12, FPS, LARGURA};

/// `PINNAME_VIDEO_CAPTURE`. Todo fluxo de uma Custom Media Source precisa desta categoria;
/// `PINNAME_VIDEO_PREVIEW` não é suportado.
const PINNAME_VIDEO_CAPTURE: GUID = GUID::from_u128(0xfb6c4281_0353_11d1_905f_0000c0cc16ba);
/// `MFFrameSourceTypes_Color`.
const FRAMESOURCE_COLOR: u32 = 0x0001;

#[derive(PartialEq, Clone, Copy, Debug)]
enum Situacao {
    Parada,
    Rodando,
    Desligada,
}

pub struct EstadoFonte {
    situacao: Situacao,
    fluxo: Option<IMFMediaStream2>,
    estado_fluxo: Option<Arc<Mutex<EstadoFluxo>>>,
    pd: Option<IMFPresentationDescriptor>,
    ja_anunciou_fluxo: bool,
}

#[implement(IMFMediaSourceEx, IMFGetService, IKsControl, IMFActivate)]
pub struct Fonte {
    estado: Arc<Mutex<EstadoFonte>>,
    fila: IMFMediaEventQueue,
    atributos: IMFAttributes,
    provedor: Arc<Provedor>,
}

/// A chave do **nome amigável** no repositório de atributos da fonte.
///
/// Escrita pelo valor, e não pelo símbolo do cabeçalho, porque foi assim que ela foi identificada
/// nesta casa: o despejo de 09/09/2026 (`docs/bancada.md` §8.59) mostrou este GUID carregando
/// exatamente o nome que passamos ao `MFCreateVirtualCamera` — `"QuallAtributos"` —, dentro do
/// `svchost` do Frame Server, em `ActivateObject` e de novo no `Start`. O símbolo que o
/// `windows-rs` exporta com este valor é `MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME`; usar a constante
/// do crate seria mais bonito e diria **menos**, porque o que autoriza este código é a medida, não
/// o nome.
const NOME_AMIGAVEL_DA_CAMERA: GUID = GUID::from_u128(0x60d0_e559_52f8_4fa2_bbce_acdb_34a8_ec01);

/// O nome da câmera que esta instância está servindo, se a plataforma tiver dito qual é.
///
/// Devolve `None` quando o repositório não tem a chave — o caso de quem instanciou a fonte
/// **direto pelo CLSID**, sem passar pelo Frame Server. Aí o cano é o [`CANO_SEM_NOME`].
fn nome_da_camera(a: &IMFAttributes) -> Option<String> {
    unsafe {
        let mut ps = PWSTR::null();
        let mut n = 0u32;
        if a.GetAllocatedString(&NOME_AMIGAVEL_DA_CAMERA, &mut ps, &mut n).is_err() {
            return None;
        }
        let s = ps.to_string().ok();
        CoTaskMemFree(Some(ps.0 as *const core::ffi::c_void));
        s.filter(|s| !s.is_empty())
    }
}

/// Constrói a fonte já ligada ao seu fluxo e devolve a interface./// Constrói a fonte já ligada ao seu fluxo e devolve a interface.
///
/// A construção acontece **fora** dos métodos COM de propósito: fonte e fluxo referenciam um ao
/// outro, e é aqui — com os dois `Arc` de estado ainda na mão — que dá para fechar o laço sem
/// precisar converter `self` em interface de dentro de um método.
pub fn criar() -> Result<IMFMediaSourceEx> {
    let provedor = Provedor::novo();

    let tipo = tipo_de_midia()?;
    let descritor = descritor_de_fluxo(&tipo)?;

    let fila_fonte: IMFMediaEventQueue = unsafe { MFCreateEventQueue() }?;
    let fila_fluxo: IMFMediaEventQueue = unsafe { MFCreateEventQueue() }?;

    let mut atributos: Option<IMFAttributes> = None;
    unsafe { MFCreateAttributes(&mut atributos, 4) }?;
    let atributos = atributos.ok_or_else(|| windows::core::Error::from(E_INVALIDARG))?;

    let estado_fonte = Arc::new(Mutex::new(EstadoFonte {
        situacao: Situacao::Parada,
        fluxo: None,
        estado_fluxo: None,
        pd: None,
        ja_anunciou_fluxo: false,
    }));

    let fonte = Fonte {
        estado: estado_fonte.clone(),
        fila: fila_fonte,
        atributos,
        provedor: provedor.clone(),
    };
    let iface: IMFMediaSourceEx = fonte.into();

    let estado_fluxo = Arc::new(Mutex::new(EstadoFluxo {
        rodando: false,
        fonte: Some(iface.cast()?),
        desligado: false,
    }));
    let fluxo = Fluxo {
        estado: estado_fluxo.clone(),
        fila: fila_fluxo,
        descritor: descritor.clone(),
        provedor,
        relogio: Arc::new(AtomicU64::new(0)),
        ritmo: Arc::new(Mutex::new(crate::ritmo::RitmoSemCano::novo())),
        entregues: Arc::new(AtomicU64::new(0)),
    };
    let ifluxo: IMFMediaStream2 = fluxo.into();

    let pd: IMFPresentationDescriptor =
        unsafe { MFCreatePresentationDescriptor(Some(&[Some(descritor)])) }?;
    unsafe { pd.SelectStream(0) }?;

    {
        let mut e = estado_fonte.lock().unwrap();
        e.fluxo = Some(ifluxo);
        e.estado_fluxo = Some(estado_fluxo);
        e.pd = Some(pd);
    }

    diga!("fonte de mídia criada ({LARGURA}x{ALTURA} NV12 {FPS}fps)");
    Ok(iface)
}

fn tipo_de_midia() -> Result<IMFMediaType> {
    let t: IMFMediaType = unsafe { MFCreateMediaType() }?;
    unsafe {
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        t.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
        t.SetUINT32(&MF_MT_SAMPLE_SIZE, BYTES_NV12 as u32)?;
        t.SetUINT32(&MF_MT_DEFAULT_STRIDE, LARGURA)?;
        // Faixa limitada, para casar com o que o pipeline do Quall já declara em
        // `docs/contrato-sidecar.md` (`color_range: "limited"`). Anunciar completa aqui e
        // entregar limitada é exatamente o defeito visual sutil que aquele documento existe
        // para evitar.
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, par(LARGURA, ALTURA))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, par(FPS, 1))?;
        t.SetUINT64(&MF_MT_FRAME_RATE_RANGE_MIN, par(FPS, 1))?;
        t.SetUINT64(&MF_MT_FRAME_RATE_RANGE_MAX, par(FPS, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, par(1, 1))?;
    }
    Ok(t)
}

fn descritor_de_fluxo(tipo: &IMFMediaType) -> Result<IMFStreamDescriptor> {
    let sd: IMFStreamDescriptor =
        unsafe { MFCreateStreamDescriptor(0, &[Some(tipo.clone())]) }?;
    unsafe {
        sd.GetMediaTypeHandler()?.SetCurrentMediaType(tipo)?;
        // Obrigatórios pelo contrato do Frame Server.
        sd.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, 0)?;
        sd.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
        // Recomendados: sem eles o Frame Server adivinha, e adivinhar aqui é como o vídeo some.
        sd.SetUINT32(&MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, FRAMESOURCE_COLOR)?;
        // 1 = compartilhável. É o que permite dois apps abrirem a câmera ao mesmo tempo — o
        // caso real de "OBS mostrando e o navegador em chamada".
        sd.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
    }
    Ok(sd)
}

fn par(a: u32, b: u32) -> u64 {
    ((a as u64) << 32) | (b as u64)
}

impl Fonte {
    fn checar_viva(&self) -> Result<()> {
        if self.estado.lock().unwrap().situacao == Situacao::Desligada {
            return Err(MF_E_SHUTDOWN.into());
        }
        Ok(())
    }
}

impl IMFMediaEventGenerator_Impl for Fonte_Impl {
    fn GetEvent(&self, dwflags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        unsafe { self.fila.GetEvent(dwflags.0 as u32) }
    }
    fn BeginGetEvent(
        &self,
        pcallback: Ref<IMFAsyncCallback>,
        punkstate: Ref<windows::core::IUnknown>,
    ) -> Result<()> {
        unsafe { self.fila.BeginGetEvent(pcallback.ok()?, punkstate.as_ref()) }
    }
    fn EndGetEvent(&self, presult: Ref<IMFAsyncResult>) -> Result<IMFMediaEvent> {
        unsafe { self.fila.EndGetEvent(presult.ok()?) }
    }
    fn QueueEvent(
        &self,
        met: u32,
        guidextendedtype: *const GUID,
        hrstatus: HRESULT,
        pvvalue: *const windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
    ) -> Result<()> {
        unsafe {
            self.fila
                .QueueEventParamVar(met, guidextendedtype, hrstatus, pvvalue)
        }
    }
}

impl IMFMediaSource_Impl for Fonte_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        self.checar_viva()?;
        // Ao vivo: sem busca, sem pausa, sem duração. É o que uma câmera é.
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> Result<IMFPresentationDescriptor> {
        self.checar_viva()?;
        let e = self.estado.lock().unwrap();
        let pd = e.pd.as_ref().ok_or(windows::core::Error::from(E_INVALIDARG))?;
        // Cópia: o pipeline seleciona fluxos no descritor que recebe, e não pode mexer no nosso.
        unsafe { pd.Clone() }
    }

    fn Start(
        &self,
        pdescritor: Ref<IMFPresentationDescriptor>,
        pguidtimeformat: *const GUID,
        pvarstartposition: *const windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
    ) -> Result<()> {
        self.checar_viva()?;
        // Rede de segurança: nem todo caminho passa por `ActivateObject` — quem instancia a fonte
        // direto pelo CLSID não passa. `identificar` é idempotente, então chamar duas vezes é de
        // graça, e não chamar aqui seria uma fonte sem cano nenhum.
        self.provedor.identificar(nome_da_camera(&self.atributos).as_deref());
        let pd = pdescritor.ok()?;
        if !pguidtimeformat.is_null() && unsafe { *pguidtimeformat } != GUID::zeroed() {
            return Err(MF_E_UNSUPPORTED_TIME_FORMAT.into());
        }

        let (fluxo, estado_fluxo, primeira_vez) = {
            let mut e = self.estado.lock().unwrap();
            let f = e.fluxo.clone().ok_or(windows::core::Error::from(E_INVALIDARG))?;
            let ef = e.estado_fluxo.clone().ok_or(windows::core::Error::from(E_INVALIDARG))?;
            let primeira = !e.ja_anunciou_fluxo;
            e.ja_anunciou_fluxo = true;
            e.situacao = Situacao::Rodando;
            (f, ef, primeira)
        };

        // O pipeline pode ter deselecionado o fluxo; se deselecionou, não há o que iniciar.
        let mut selecionado = windows::core::BOOL(0);
        let mut sd: Option<IMFStreamDescriptor> = None;
        unsafe { pd.GetStreamDescriptorByIndex(0, &mut selecionado, &mut sd) }?;
        if !selecionado.as_bool() {
            diga!("Start com o fluxo 0 deselecionado — nada a iniciar");
            return Ok(());
        }

        estado_fluxo.lock().unwrap().rodando = true;
        unsafe {
            let como_unk: windows::core::IUnknown = fluxo.cast()?;
            let evento = if primeira_vez { MENewStream } else { MEUpdatedStream };
            self.fila
                .QueueEventParamUnk(evento.0 as u32, &GUID::zeroed(), S_OK, &como_unk)?;
            // MEStreamStarted vai na fila do **fluxo**, não na da fonte. Trocar as duas é o
            // tipo de erro que deixa o app esperando para sempre sem nenhum erro aparecer.
            //
            // E vai com um PROPVARIANT de verdade (a posição de início), não com ponteiro nulo:
            // `QueueEventParamVar` aceita nulo em algumas implementações e não em outras, e um
            // evento que não entra é indistinguível de um evento que ninguém escutou.
            let vazio = windows::Win32::System::Com::StructuredStorage::PROPVARIANT::default();
            let pos = if pvarstartposition.is_null() {
                &vazio as *const _
            } else {
                pvarstartposition
            };
            fluxo.QueueEvent(MEStreamStarted.0 as u32, &GUID::zeroed(), S_OK, pos)?;
            self.fila.QueueEventParamVar(
                MESourceStarted.0 as u32,
                &GUID::zeroed(),
                S_OK,
                pos,
            )?;
        }
        diga!("Start: fonte rodando (primeira_vez={primeira_vez})");
        Ok(())
    }

    fn Stop(&self) -> Result<()> {
        self.checar_viva()?;
        let (fluxo, estado_fluxo) = {
            let mut e = self.estado.lock().unwrap();
            e.situacao = Situacao::Parada;
            (e.fluxo.clone(), e.estado_fluxo.clone())
        };
        if let Some(ef) = estado_fluxo {
            ef.lock().unwrap().rodando = false;
        }
        unsafe {
            if let Some(f) = &fluxo {
                f.QueueEvent(MEStreamStopped.0 as u32, &GUID::zeroed(), S_OK, std::ptr::null())?;
            }
            self.fila.QueueEventParamVar(
                MESourceStopped.0 as u32,
                &GUID::zeroed(),
                S_OK,
                std::ptr::null(),
            )?;
        }
        diga!("Stop");
        Ok(())
    }

    fn Pause(&self) -> Result<()> {
        // Fonte ao vivo não pausa; `GetCharacteristics` não anuncia `CAN_PAUSE`.
        Err(MF_E_INVALID_STATE_TRANSITION.into())
    }

    fn Shutdown(&self) -> Result<()> {
        let (fluxo, estado_fluxo) = {
            let mut e = self.estado.lock().unwrap();
            if e.situacao == Situacao::Desligada {
                return Ok(());
            }
            e.situacao = Situacao::Desligada;
            e.pd = None;
            // Desfaz o laço fonte↔fluxo aqui. COM não tem referência fraca: sem este ponto, os
            // dois objetos se seguram para sempre e a DLL nunca descarrega.
            let f = e.fluxo.take();
            let ef = e.estado_fluxo.take();
            (f, ef)
        };
        if let Some(ef) = &estado_fluxo {
            let mut g = ef.lock().unwrap();
            g.desligado = true;
            g.rodando = false;
            g.fonte = None;
        }
        drop(fluxo);
        self.provedor.parar();
        unsafe { self.fila.Shutdown() }?;
        diga!("Shutdown");
        Ok(())
    }
}

impl IMFMediaSourceEx_Impl for Fonte_Impl {
    fn GetSourceAttributes(&self) -> Result<IMFAttributes> {
        self.checar_viva()?;
        Ok(self.atributos.clone())
    }

    fn GetStreamAttributes(&self, dwstreamidentifier: u32) -> Result<IMFAttributes> {
        self.checar_viva()?;
        if dwstreamidentifier != 0 {
            return Err(E_INVALIDARG.into());
        }
        let e = self.estado.lock().unwrap();
        let f = e.fluxo.clone().ok_or(windows::core::Error::from(E_INVALIDARG))?;
        drop(e);
        // O descritor de fluxo **é** o repositório de atributos do fluxo.
        let sd = unsafe { f.GetStreamDescriptor() }?;
        sd.cast()
    }

    fn SetD3DManager(&self, _pmanager: Ref<windows::core::IUnknown>) -> Result<()> {
        // Aceito e ignorado: esta fonte entrega buffers de memória de sistema. Recusar com
        // E_NOTIMPL faz alguns consumidores desistirem do caminho todo; aceitar e entregar
        // memória de sistema é o comportamento que o pipeline sabe tratar.
        diga!("SetD3DManager (ignorado — entregamos buffer de sistema)");
        Ok(())
    }
}

impl IMFGetService_Impl for Fonte_Impl {
    fn GetService(
        &self,
        _guidservice: *const GUID,
        _riid: *const GUID,
        _ppvobject: *mut *mut core::ffi::c_void,
    ) -> Result<()> {
        // Obrigatória de existir, livre de oferecer serviço.
        Err(MF_E_UNSUPPORTED_SERVICE.into())
    }
}

impl IKsControl_Impl for Fonte_Impl {
    fn KsProperty(
        &self,
        _property: *const KSIDENTIFIER,
        _propertylength: u32,
        _propertydata: *mut core::ffi::c_void,
        _datalength: u32,
        _bytesreturned: *mut u32,
    ) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn KsMethod(
        &self,
        _method: *const KSIDENTIFIER,
        _methodlength: u32,
        _methoddata: *mut core::ffi::c_void,
        _datalength: u32,
        _bytesreturned: *mut u32,
    ) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn KsEvent(
        &self,
        _event: *const KSIDENTIFIER,
        _eventlength: u32,
        _eventdata: *mut core::ffi::c_void,
        _datalength: u32,
        _bytesreturned: *mut u32,
    ) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
}

// ---------------------------------------------------------------------------------------------
// IMFActivate + IMFAttributes
//
// **Não é opcional para uma câmera virtual, ao contrário do que a documentação da Custom Media
// Source sugere.** Medido no Dell: `IMFVirtualCamera::Start` faz `CoCreateInstance` do CLSID e
// pede `IMFActivate` (IID `{7FEE9E9A-4A89-47A6-899C-B6A53A70FB67}`); sem ela, `Start` devolve
// `E_NOINTERFACE` (0x80004002) e a câmera é criada mas nunca liga. A documentação chama isso de
// "extensão" desde o Windows 10 1809; para `MFCreateVirtualCamera` é requisito.
//
// `IMFAttributes` inteira é delegada ao mesmo repositório que `GetSourceAttributes` devolve —
// que é exatamente o que a documentação recomenda quando as duas interfaces moram no mesmo
// objeto: é por esse repositório que o pipeline entrega o link simbólico do dispositivo.
// ---------------------------------------------------------------------------------------------

impl IMFActivate_Impl for Fonte_Impl {
    fn ActivateObject(
        &self,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> Result<()> {
        // **É aqui que a plataforma diz qual câmera esta instância é** — medido em §8.59, e é
        // o primeiro ponto da vida em que o repositório de atributos tem alguma coisa dentro.
        // Identificar aqui, e não no `Start`, dá ao fio leitor os milissegundos de vantagem que
        // separam "primeira imagem em 13 ms" de "primeira imagem depois do primeiro quadro".
        self.provedor.identificar(nome_da_camera(&self.atributos).as_deref());
        unsafe { self.QueryInterface(riid, ppv).ok() }
    }
    fn ShutdownObject(&self) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn DetachObject(&self) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
}

type Propv = windows::Win32::System::Com::StructuredStorage::PROPVARIANT;

impl IMFAttributes_Impl for Fonte_Impl {
    fn GetItem(&self, k: *const GUID, v: *mut Propv) -> Result<()> {
        unsafe { self.atributos.GetItem(k, Some(v)) }
    }
    fn GetItemType(&self, k: *const GUID) -> Result<MF_ATTRIBUTE_TYPE> {
        unsafe { self.atributos.GetItemType(k) }
    }
    fn CompareItem(&self, k: *const GUID, v: *const Propv) -> Result<windows::core::BOOL> {
        unsafe { self.atributos.CompareItem(k, v) }
    }
    fn Compare(&self, ptheirs: Ref<IMFAttributes>, t: MF_ATTRIBUTES_MATCH_TYPE) -> Result<windows::core::BOOL> {
        unsafe { self.atributos.Compare(ptheirs.ok()?, t) }
    }
    fn GetUINT32(&self, k: *const GUID) -> Result<u32> {
        unsafe { self.atributos.GetUINT32(k) }
    }
    fn GetUINT64(&self, k: *const GUID) -> Result<u64> {
        unsafe { self.atributos.GetUINT64(k) }
    }
    fn GetDouble(&self, k: *const GUID) -> Result<f64> {
        unsafe { self.atributos.GetDouble(k) }
    }
    fn GetGUID(&self, k: *const GUID) -> Result<GUID> {
        unsafe { self.atributos.GetGUID(k) }
    }
    fn GetStringLength(&self, k: *const GUID) -> Result<u32> {
        unsafe { self.atributos.GetStringLength(k) }
    }
    fn GetString(&self, k: *const GUID, p: windows::core::PWSTR, n: u32, out: *mut u32) -> Result<()> {
        unsafe {
            let fatia = std::slice::from_raw_parts_mut(p.0, n as usize);
            self.atributos.GetString(k, fatia, Some(out))
        }
    }
    fn GetAllocatedString(&self, k: *const GUID, p: *mut windows::core::PWSTR, n: *mut u32) -> Result<()> {
        unsafe { self.atributos.GetAllocatedString(k, p, n) }
    }
    fn GetBlobSize(&self, k: *const GUID) -> Result<u32> {
        unsafe { self.atributos.GetBlobSize(k) }
    }
    fn GetBlob(&self, k: *const GUID, p: *mut u8, n: u32, out: *mut u32) -> Result<()> {
        unsafe {
            let fatia = std::slice::from_raw_parts_mut(p, n as usize);
            self.atributos.GetBlob(k, fatia, Some(out))
        }
    }
    fn GetAllocatedBlob(&self, k: *const GUID, p: *mut *mut u8, n: *mut u32) -> Result<()> {
        unsafe { self.atributos.GetAllocatedBlob(k, p, n) }
    }
    fn GetUnknown(&self, k: *const GUID, riid: *const GUID, ppv: *mut *mut core::ffi::c_void) -> Result<()> {
        unsafe {
            let unk: windows::core::IUnknown = self.atributos.GetUnknown(k)?;
            unk.query(&*riid, ppv).ok()
        }
    }
    fn SetItem(&self, k: *const GUID, v: *const Propv) -> Result<()> {
        unsafe { self.atributos.SetItem(k, v) }
    }
    fn DeleteItem(&self, k: *const GUID) -> Result<()> {
        unsafe { self.atributos.DeleteItem(k) }
    }
    fn DeleteAllItems(&self) -> Result<()> {
        unsafe { self.atributos.DeleteAllItems() }
    }
    fn SetUINT32(&self, k: *const GUID, v: u32) -> Result<()> {
        unsafe { self.atributos.SetUINT32(k, v) }
    }
    fn SetUINT64(&self, k: *const GUID, v: u64) -> Result<()> {
        unsafe { self.atributos.SetUINT64(k, v) }
    }
    fn SetDouble(&self, k: *const GUID, v: f64) -> Result<()> {
        unsafe { self.atributos.SetDouble(k, v) }
    }
    fn SetGUID(&self, k: *const GUID, v: *const GUID) -> Result<()> {
        unsafe { self.atributos.SetGUID(k, v) }
    }
    fn SetString(&self, k: *const GUID, v: &windows::core::PCWSTR) -> Result<()> {
        unsafe { self.atributos.SetString(k, *v) }
    }
    fn SetBlob(&self, k: *const GUID, p: *const u8, n: u32) -> Result<()> {
        unsafe {
            let fatia = std::slice::from_raw_parts(p, n as usize);
            self.atributos.SetBlob(k, fatia)
        }
    }
    fn SetUnknown(&self, k: *const GUID, v: Ref<windows::core::IUnknown>) -> Result<()> {
        unsafe { self.atributos.SetUnknown(k, v.as_ref()) }
    }
    fn LockStore(&self) -> Result<()> {
        unsafe { self.atributos.LockStore() }
    }
    fn UnlockStore(&self) -> Result<()> {
        unsafe { self.atributos.UnlockStore() }
    }
    fn GetCount(&self) -> Result<u32> {
        unsafe { self.atributos.GetCount() }
    }
    fn GetItemByIndex(&self, i: u32, k: *mut GUID, v: *mut Propv) -> Result<()> {
        unsafe { self.atributos.GetItemByIndex(i, k, Some(v)) }
    }
    fn CopyAllItems(&self, pdest: Ref<IMFAttributes>) -> Result<()> {
        unsafe { self.atributos.CopyAllItems(pdest.ok()?) }
    }
}
