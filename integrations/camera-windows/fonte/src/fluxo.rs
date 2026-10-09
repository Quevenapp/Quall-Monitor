//! O fluxo de vídeo da fonte de mídia (`IMFMediaStream2`).
//!
//! O Frame Server exige `IMFMediaEventGenerator` + `IMFMediaStream` + `IMFMediaStream2` em cada
//! fluxo; as duas primeiras são herdadas pela terceira, mas todas precisam ter implementação.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{implement, Interface, Ref, Result, GUID};
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::MediaFoundation::*;

use crate::diga;
use crate::quadros::{Provedor, BYTES_NV12, FPS};

/// Duração de um quadro em unidades de 100 ns, que é o relógio do Media Foundation.
const DURACAO_100NS: i64 = 10_000_000 / FPS as i64;

pub struct EstadoFluxo {
    pub rodando: bool,
    pub fonte: Option<IMFMediaSource>,
    pub desligado: bool,
}

#[implement(IMFMediaStream2)]
pub struct Fluxo {
    pub estado: Arc<Mutex<EstadoFluxo>>,
    pub fila: IMFMediaEventQueue,
    pub descritor: IMFStreamDescriptor,
    pub provedor: Arc<Provedor>,
    /// Carimbo do próximo quadro, em 100 ns. Um `IMFMediaSource` ao vivo precisa entregar tempo
    /// que anda sozinho: o consumidor usa isso para ritmar a exibição.
    pub relogio: Arc<AtomicU64>,
    /// O ritmo do padrão de bancada, por trecho sem cano (`ritmo.rs`: o R4 da câmera no Windows,
    /// M51, achou a espera de `entregues / 30` s que ele conserta).
    pub ritmo: Arc<Mutex<crate::ritmo::RitmoSemCano>>,
    pub entregues: Arc<AtomicU64>,
}

impl Fluxo {
    fn desligado(&self) -> bool {
        self.estado.lock().unwrap().desligado
    }
}

impl IMFMediaEventGenerator_Impl for Fluxo_Impl {
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
        hrstatus: windows::core::HRESULT,
        pvvalue: *const windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
    ) -> Result<()> {
        unsafe {
            self.fila
                .QueueEventParamVar(met, guidextendedtype, hrstatus, pvvalue)
        }
    }
}

impl IMFMediaStream_Impl for Fluxo_Impl {
    fn GetMediaSource(&self) -> Result<IMFMediaSource> {
        let e = self.estado.lock().unwrap();
        if e.desligado {
            return Err(MF_E_SHUTDOWN.into());
        }
        e.fonte.clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }

    fn GetStreamDescriptor(&self) -> Result<IMFStreamDescriptor> {
        if self.desligado() {
            return Err(MF_E_SHUTDOWN.into());
        }
        Ok(self.descritor.clone())
    }

    /// O coração da entrega. É chamado pelo pipeline sempre que ele quer mais um quadro.
    fn RequestSample(&self, ptoken: Ref<windows::core::IUnknown>) -> Result<()> {
        {
            let e = self.estado.lock().unwrap();
            if e.desligado {
                return Err(MF_E_SHUTDOWN.into());
            }
            if !e.rodando {
                return Err(MF_E_INVALIDREQUEST.into());
            }
        }

        // Ritmo. Uma fonte ao vivo que devolve quadro na hora que pedirem gira em laço aberto e
        // come um núcleo inteiro; o pipeline pede o próximo assim que recebe o anterior.
        //
        // Com o cano conectado, quem dita o ritmo é **a chegada do quadro**, não este relógio: o
        // `proximo` bloqueia até o quadro chegar. Ritmar dos dois lados somaria fase — foi o que
        // a primeira medição desta bancada mostrou (52,8 ms de latência, ver README).
        let (bytes, do_cano) = {
            let prazo = std::time::Duration::from_millis(2_000 / FPS as u64);
            let r = self.provedor.proximo(prazo);
            if r.1 {
                self.ritmo.lock().unwrap().com_cano();
            } else {
                // Sem cano: ritma no relógio local, senão o padrão de bancada sai a milhares de
                // quadros por segundo. **Contado do começo deste trecho sem cano**, e nunca mais
                // que um intervalo de quadro (`ritmo.rs`; antes, o cano que caía depois de N
                // quadros congelava a fonte por N/30 s).
                let espera = self.ritmo.lock().unwrap().espera(std::time::Instant::now(), FPS);
                if !espera.is_zero() {
                    std::thread::sleep(espera);
                }
            }
            r
        };
        debug_assert_eq!(bytes.len(), BYTES_NV12);

        let amostra: IMFSample = unsafe { MFCreateSample() }?;
        let buffer: IMFMediaBuffer = unsafe { MFCreateMemoryBuffer(BYTES_NV12 as u32) }?;
        unsafe {
            let mut destino: *mut u8 = std::ptr::null_mut();
            buffer.Lock(&mut destino, None, None)?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), destino, BYTES_NV12);
            buffer.Unlock()?;
            buffer.SetCurrentLength(BYTES_NV12 as u32)?;
            amostra.AddBuffer(&buffer)?;

            // **Relógio do sistema, não um contador que começa em zero.**
            //
            // Medido nesta bancada: com carimbo começando em 0, consumir a fonte **direto**
            // (`CoCreateInstance` no próprio processo) funciona — 90 quadros, 33,4 ms de
            // intervalo — e consumir **pelo Frame Server** pendura o `ReadSample` para sempre,
            // sem erro nenhum. Uma câmera ao vivo carimba com `MFGetSystemTime()`, que é o mesmo
            // relógio (QPC, unidades de 100 ns) contra o qual o Frame Server agenda.
            let t = MFGetSystemTime();
            self.relogio.store(t as u64, Ordering::Relaxed);
            amostra.SetSampleTime(t)?;
            amostra.SetSampleDuration(DURACAO_100NS)?;
            if let Some(token) = ptoken.as_ref() {
                amostra.SetUnknown(&MFSampleExtension_Token, token)?;
            }
            let como_unk: windows::core::IUnknown = amostra.cast()?;
            self.fila.QueueEventParamUnk(
                MEMediaSample.0 as u32,
                &GUID::zeroed(),
                S_OK,
                &como_unk,
            )?;
        }

        let n = self.entregues.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 || n % 300 == 0 {
            let c = self.provedor.contadores();
            diga!(
                "quadro {n} entregue (origem={}) — cano: conectado={} recebidos={} entregues={} \
                 descartados={} repetidos={} idade_do_quadro={}us ts_do_host={}",
                if do_cano { "cano" } else { "padrão" },
                c.conectado,
                c.recebidos,
                c.entregues,
                c.descartados,
                c.repetidos,
                c.idade_us,
                c.ultimo_ts_us
            );
        }
        Ok(())
    }
}

impl IMFMediaStream2_Impl for Fluxo_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> Result<()> {
        let mut e = self.estado.lock().unwrap();
        if e.desligado {
            return Err(MF_E_SHUTDOWN.into());
        }
        e.rodando = value == MF_STREAM_STATE_RUNNING;
        diga!("SetStreamState({:?}) -> rodando={}", value.0, e.rodando);
        Ok(())
    }

    fn GetStreamState(&self) -> Result<MF_STREAM_STATE> {
        let e = self.estado.lock().unwrap();
        if e.desligado {
            return Err(MF_E_SHUTDOWN.into());
        }
        Ok(if e.rodando {
            MF_STREAM_STATE_RUNNING
        } else {
            MF_STREAM_STATE_STOPPED
        })
    }
}
