use std::sync::atomic::AtomicU16;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use database::migrate_database;
use logger::KndLogger;
use once_cell::sync::OnceCell;
use settings::Settings;
use test_utils::cockroach;
use test_utils::test_settings_for_database;
use test_utils::CockroachManager;
use tokio::runtime::Handle;

pub mod ldk_database;
pub mod wallet_database;

static GLOBAL_REF_COUNT: AtomicU16 = AtomicU16::new(0);

// Need to call teardown function at the end of the test if using this.
async fn global_cockroach() -> &'static (Settings, Mutex<CockroachManager>) {
    GLOBAL_REF_COUNT.fetch_add(1, Ordering::AcqRel);
    static INSTANCE: OnceCell<(Settings, Mutex<CockroachManager>)> = OnceCell::new();
    INSTANCE.get_or_init(|| {
        KndLogger::init("test", log::LevelFilter::Debug);
        tokio::task::block_in_place(move || {
            Handle::current().block_on(async move {
                let mut cockroach = cockroach!();
                cockroach.start().await;
                let settings = test_settings_for_database(&cockroach);
                migrate_database(&settings).await.unwrap();
                (settings, Mutex::new(cockroach))
            })
        })
    })
}

pub async fn teardown() {
    if GLOBAL_REF_COUNT.fetch_sub(1, Ordering::AcqRel) == 1 {
        let mut lock = global_cockroach().await.1.lock().unwrap();
        lock.kill();
    }
}

