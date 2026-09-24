//! Veille sur les périphériques **par défaut** — abonnement COM
//! `IMMNotificationClient` (MMDevice API), en remplacement du sondage
//! PowerShell `overview` toutes les 10 s : **zéro processus en
//! arrière-plan**, détection instantanée.
//!
//! Contrat (MMDevice API, standard — ni API interne ni reverse-engineering) :
//! - `IMMDeviceEnumerator::RegisterEndpointNotificationCallback` (slot **6**
//!   de la vtable ; Unregister = 7) prend un objet COM implémentant
//!   `IMMNotificationClient`, IID `7991EEC9-7E89-4D85-8390-6C703CEC60C0` :
//!   vtable 3 `OnDeviceStateChanged`, 4 `OnDeviceAdded`, 5 `OnDeviceRemoved`,
//!   6 `OnDefaultDeviceChanged(flow, role, id)`, 7 `OnPropertyValueChanged`.
//! - **Aucun message loop à pomper** : l'enregistrement est fait depuis un
//!   thread MTA dédié (`audio-devices-watch`, `RO_INIT_MULTITHREADED`) et les
//!   rappels sont livrés par des threads de travail COM. Chaque callback est
//!   enfermé dans `catch_unwind` (aucun unwind à travers une FFI) et se
//!   limite à un envoi sur un canal **non borné** (jamais bloquant) — la
//!   sauvegarde (PowerShell, ~2 s) tourne sur le thread de la veille.
//! - un même changement de défaut arrive en rafale (rôles console,
//!   multimédia, communications) avec la même valeur → `WatchState` ne
//!   notifie qu'une seule fois.
//! - l'énumérateur reste vivant tant que le thread vit ; pas de
//!   désabonnement — la fin du processus suffit.
//!
//! Le thread est abonné **quelle que soit** l'option `watchDevices` au
//! démarrage : chaque événement reverifie `settings::load()`, donc
//! activation/désactivation prennent effet immédiatement (l'ancien sondage
//! exigeait un redémarrage pour activer).

use super::devices::{device_enumerator, device_id};
use super::ffi::{vtable_slot, ComRef, Guid, PropertyKey};
use super::Flow;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, MutexGuard};

// ---------------------------------------------------------------------------
// Constantes COM
// ---------------------------------------------------------------------------

/// IID_IMMNotificationClient : {7991EEC9-7E89-4D85-8390-6C703CEC60C0}.
const IID_IMM_NOTIFICATION_CLIENT: Guid = Guid {
    data1: 0x7991_eec9,
    data2: 0x7e89,
    data3: 0x4d85,
    data4: [0x83, 0x90, 0x6c, 0x70, 0x3c, 0xec, 0x60, 0xc0],
};
/// IID_IUnknown : {00000000-0000-0000-C000-000000000046}.
const IID_IUNKNOWN: Guid = Guid {
    data1: 0,
    data2: 0,
    data3: 0,
    data4: [0xc0, 0, 0, 0, 0, 0, 0, 0x46],
};
/// E_NOINTERFACE.
const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;
/// E_POINTER.
const E_POINTER: i32 = 0x8000_4003u32 as i32;
/// E_FAIL (repli si un callback panique — jamais d'panic à travers la FFI).
const E_FAIL: i32 = 0x8000_4005u32 as i32;

/// Message du callback vers le thread de la veille :
/// `(EDataFlow, nouvel identifiant du périphérique par défaut)`.
type WatchMsg = (i32, String);

/// Émetteur publié pour les callbacks (threads MTA). `Mutex` (et non `Sender`
/// directement) car `Sender` n'est pas `Sync` — le verrou n'est tenu que le
/// temps d'un `send` sur canal non borné : jamais bloquant, aucun risque de
/// deadlock avec le thread consommateur (qui ne verrouille jamais ici).
static TX: Mutex<Option<Sender<WatchMsg>>> = Mutex::new(None);

fn tx_lock() -> MutexGuard<'static, Option<Sender<WatchMsg>>> {
    TX.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// Objet COM IMMNotificationClient (statique, vtable en Rust)
// ---------------------------------------------------------------------------

/// Vtable de `IMMNotificationClient` (IUnknown + 5 rappels), layout `repr(C)`
/// identique à la vtable C attendue par la MMDevice API.
#[repr(C)]
struct NotificationVtbl {
    query_interface: extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    add_ref: extern "system" fn(*mut c_void) -> u32,
    release: extern "system" fn(*mut c_void) -> u32,
    on_device_state_changed: extern "system" fn(*mut c_void, *const u16, u32),
    on_device_added: extern "system" fn(*mut c_void, *const u16),
    on_device_removed: extern "system" fn(*mut c_void, *const u16),
    on_default_device_changed: extern "system" fn(*mut c_void, i32, i32, *const u16),
    on_property_value_changed: extern "system" fn(*mut c_void, *const u16, *const PropertyKey),
}

static VTBL: NotificationVtbl = NotificationVtbl {
    query_interface,
    add_ref,
    release,
    on_device_state_changed,
    on_device_added,
    on_device_removed,
    on_default_device_changed,
    on_property_value_changed,
};

/// Instance unique — **statique, jamais libérée** : le compteur de
/// références est purement conventionnel (la MMDevice API AddRef/Release
/// pendant la vie de l'abonnement ; la fin du processus suffit à tout
/// libérer). Premier champ = pointeur de vtable (layout COM `repr(C)`).
#[repr(C)]
struct NotificationClient {
    vtable: &'static NotificationVtbl,
    refcount: AtomicU32,
}

static CLIENT: NotificationClient = NotificationClient {
    vtable: &VTBL,
    refcount: AtomicU32::new(1),
};

fn client_of(this: *mut c_void) -> &'static NotificationClient {
    // SAFETY : `this` provient de la MMDevice API et pointe vers CLIENT
    // (seul objet implémentant cette vtable côté client).
    unsafe { &*(this as *const NotificationClient) }
}

/// Lit une chaîne UTF-16 nulle native (identifiant de périphérique).
fn wide_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    // Plafond de sécurité : un identifiant MMDevice reste très loin de 4096.
    while len < 4096 && unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) })
}

extern "system" fn query_interface(
    this: *mut c_void,
    riid: *const Guid,
    out: *mut *mut c_void,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if riid.is_null() || out.is_null() {
            return E_POINTER;
        }
        // SAFETY : pointeurs validés ci-dessus.
        let iid = unsafe { *riid };
        if iid == IID_IMM_NOTIFICATION_CLIENT || iid == IID_IUNKNOWN {
            // Même objet pour les trois interfaces (héritage simple).
            unsafe { *out = this; }
            add_ref(this);
            0
        } else {
            unsafe { *out = std::ptr::null_mut(); }
            E_NOINTERFACE
        }
    }))
    .unwrap_or(E_FAIL)
}

extern "system" fn add_ref(this: *mut c_void) -> u32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client_of(this).refcount.fetch_add(1, Ordering::Relaxed) + 1
    }))
    .unwrap_or(1)
}

extern "system" fn release(this: *mut c_void) -> u32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Objet statique : on borne le compteur sans jamais libérer.
        client_of(this)
            .refcount
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            })
            .unwrap_or(0)
            .saturating_sub(1)
    }))
    .unwrap_or(0)
}

// Les trois rappels « périphérique » et le rappel « propriété » n'intéressent
// pas la veille (seul le défaut par défaut déclenche une sauvegarde).
extern "system" fn on_device_state_changed(_this: *mut c_void, _id: *const u16, _state: u32) {}
extern "system" fn on_device_added(_this: *mut c_void, _id: *const u16) {}
extern "system" fn on_device_removed(_this: *mut c_void, _id: *const u16) {}
extern "system" fn on_property_value_changed(
    _this: *mut c_void,
    _id: *const u16,
    _key: *const PropertyKey,
) {
}

/// Cœur du callback : met à jour l'état puis notifie si la valeur change.
/// Seul `EDataFlow` compte (0 = sortie, 1 = entrée) ; `role` est ignoré —
/// les rôles d'une même bascule arrivent en rafale avec la même valeur et
/// `WatchState` déduplique.
extern "system" fn on_default_device_changed(
    _this: *mut c_void,
    flow: i32,
    _role: i32,
    device_id_ptr: *const u16,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let id = wide_to_string(device_id_ptr);
        if let Some(tx) = tx_lock().as_ref() {
            let _ = tx.send((flow, id)); // non borné : ne bloque jamais
        }
    }));
}

// ---------------------------------------------------------------------------
// Machine à états (pure, testable) + thread de la veille
// ---------------------------------------------------------------------------

/// État de comparaison des défauts. Pure : les événements arrivent en rafale
/// (trois rôles par bascule) — on ne notifie qu'un changement réel par
/// rapport à la **dernière** notification émise.
#[derive(Debug, PartialEq, Eq)]
struct WatchState {
    /// `(sortie, entrée)` : dernière valeur observée.
    current: (String, String),
    /// `(sortie, entrée)` : valeur à l'origine de la dernière notification.
    last_notified: (String, String),
}

impl WatchState {
    fn seed(playback: String, recording: String) -> Self {
        let current = (playback, recording);
        // La baseline ne déclenche rien (comme le premier échantillon de
        // l'ancien sondage) : seuls les CHANGEMENTS suivants notifient.
        let last_notified = current.clone();
        Self {
            current,
            last_notified,
        }
    }

    /// Applique `OnDefaultDeviceChanged(flow, id)` ; renvoie
    /// `Some((sortie, entrée))` si la valeur diffère de la dernière
    /// notification émise (rafale de rôles → une seule sauvegarde).
    fn accept(&mut self, flow: i32, id: String) -> Option<(String, String)> {
        match flow {
            0 => self.current.0 = id,
            1 => self.current.1 = id,
            _ => return None, // flux inconnu : ignorer
        }
        if self.current != self.last_notified {
            self.last_notified = self.current.clone();
            Some(self.current.clone())
        } else {
            None
        }
    }
}

/// IMMDeviceEnumerator [4] `GetDefaultAudioEndpoint(flow, role, **device)` —
/// sert à semer la baseline avant l'enregistrement.
fn default_device_id(enumerator: &ComRef, flow: Flow) -> String {
    type GetDefaultAudioEndpointFn =
        unsafe extern "system" fn(*mut c_void, i32, i32, *mut *mut c_void) -> i32;
    // SAFETY : slot 4 vérifié (memory.md vtable map).
    let get_default: GetDefaultAudioEndpointFn =
        unsafe { std::mem::transmute(vtable_slot(enumerator.ptr(), 4)) };
    let mut device_ptr = std::ptr::null_mut();
    let hr = unsafe {
        get_default(
            enumerator.ptr(),
            flow.value(),
            0, // eConsole
            &mut device_ptr,
        )
    };
    if hr < 0 || device_ptr.is_null() {
        return String::new();
    }
    let device = ComRef::new(device_ptr);
    device_id(&device).unwrap_or_default()
}

fn watch_loop<F>(rx: Receiver<WatchMsg>, tx: Sender<WatchMsg>, mut on_change: F)
where
    F: FnMut((String, String)) + Send + 'static,
{
    let enumerator = match device_enumerator() {
        Ok(enumerator) => enumerator,
        Err(e) => {
            crate::logging::warn(&format!("veille périphériques : {e}"));
            return;
        }
    };

    // Baseline AVANT l'enregistrement : aucun événement ne peut précéder la
    // valeur lue juste avant l'abonnement (fenêtre identique à l'ancien
    // premier échantillon).
    let mut state = WatchState::seed(
        default_device_id(&enumerator, Flow::Output),
        default_device_id(&enumerator, Flow::Input),
    );

    // Émetteur publié AVANT l'enregistrement : aucun événement perdu entre
    // Register… et la mise en place du canal.
    *tx_lock() = Some(tx);

    // IMMDeviceEnumerator [6] RegisterEndpointNotificationCallback.
    type RegisterFn = unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32;
    // SAFETY : slot 6 de la MMDevice API (documenté).
    let register: RegisterFn = unsafe { std::mem::transmute(vtable_slot(enumerator.ptr(), 6)) };
    let hr = unsafe {
        register(
            enumerator.ptr(),
            &CLIENT as *const NotificationClient as *mut c_void,
        )
    };
    if hr < 0 {
        *tx_lock() = None;
        crate::logging::warn(&format!(
            "abonnement notifications audio impossible (0x{:08X})",
            hr as u32
        ));
        return;
    }

    // Ce thread vit jusqu'à la fin du processus : appartement COM + Drain de
    // la queue d'événements ; l'énumérateur local reste référencé (l'abonnement
    // reste valide). Pas de désabonnement — la fin du processus suffit.
    let _keep_registered = &enumerator;
    for (flow, id) in rx {
        if let Some((playback, recording)) = state.accept(flow, id) {
            on_change((playback, recording));
        }
    }
}

// ---------------------------------------------------------------------------
// API publique
// ---------------------------------------------------------------------------

/// Abonne une veille event-driven sur les périphériques **par défaut** et
/// lance le thread `audio-devices-watch`. `on_change((sortie, entrée))` est
/// appelé (thread de la veille, jamais un callback COM) à chaque changement
/// réel des défauts — **après** relecture de `settings.watch_devices` par
/// l'appelant si besoin. Idempotent : un second appel est un no-op.
pub fn spawn_default_device_watch<F>(on_change: F) -> Result<(), String>
where
    F: FnMut((String, String)) + Send + 'static,
{
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let (tx, rx) = mpsc::channel::<WatchMsg>();
    let spawned = std::thread::Builder::new()
        .name("audio-devices-watch".into())
        .spawn(move || {
            // RO_INIT_MULTITHREADED = 1 : les rappels MMDevice sont livrés
            // sur des threads MTA — aucun message loop à pomper ici.
            let hr = unsafe { super::ffi::RoInitialize(1) };
            watch_loop(rx, tx, on_change);
            if hr >= 0 {
                unsafe { super::ffi::RoUninitialize() };
            }
        });
    match spawned {
        Ok(_) => Ok(()),
        Err(e) => {
            STARTED.store(false, Ordering::SeqCst);
            Err(format!("impossible de lancer la veille périphériques : {e}"))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (logique pure — aucun COM dans les tests)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::WatchState;

    #[test]
    fn seed_baseline_ne_notifie_pas() {
        let mut state = WatchState::seed("A".into(), "M".into());
        // Même valeur que la baseline (rafale de rôles) → aucune notification.
        assert_eq!(state.accept(0, "A".into()), None);
        assert_eq!(state.accept(1, "M".into()), None);
    }

    #[test]
    fn changement_sortie_notifie_une_fois_malgre_les_roles() {
        let mut state = WatchState::seed("A".into(), "M".into());
        // Console : changement réel → notification.
        assert_eq!(
            state.accept(0, "B".into()),
            Some(("B".into(), "M".into()))
        );
        // Multimédia + communications (même valeur, même bascule) → non.
        assert_eq!(state.accept(0, "B".into()), None);
        assert_eq!(state.accept(0, "B".into()), None);
    }

    #[test]
    fn changement_entree_ne_touchepas_la_sortie() {
        let mut state = WatchState::seed("A".into(), "M".into());
        assert_eq!(
            state.accept(1, "N".into()),
            Some(("A".into(), "N".into()))
        );
    }

    #[test]
    fn flux_inconnu_ignore() {
        let mut state = WatchState::seed("A".into(), "M".into());
        assert_eq!(state.accept(2, "X".into()), None);
        assert_eq!(state.accept(-1, "X".into()), None);
        assert_eq!(state.current, ("A".into(), "M".into()));
    }

    #[test]
    fn aller_retour_notifie_chaque_fois() {
        let mut state = WatchState::seed("A".into(), "M".into());
        assert!(state.accept(0, "B".into()).is_some());
        assert!(state.accept(0, "A".into()).is_some());
        // Puis rafale sur la valeur finale → plus rien.
        assert_eq!(state.accept(0, "A".into()), None);
    }
}
