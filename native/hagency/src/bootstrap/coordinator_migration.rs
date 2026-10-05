//! Offline resource-owner migration. The initialized state's two owner locks
//! exclude running custody/domain writers; this is not a Palpo capability.
use hagency_store::{DomainRepository, Repository, private};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Audit the current native database without disclosing row contents.
    Inventory,
    /// Map existing allocations into bounded engagement grants atomically.
    Adopt {
        /// Private JSON plan bound to the Inventory source digest.
        #[arg(long)]
        file: PathBuf,
    },
}

pub fn run(state: &Path, command: Command) -> Result<serde_json::Value, hagency_store::Error> {
    private::read_secret(&state.join("operator.token"))?;
    let _custody = Repository::lock_for_migration(state)?;
    let mut domain = DomainRepository::open_for_migration(state)?;
    match command {
        Command::Inventory => domain.coordinator_migration_inventory(),
        Command::Adopt { file } => {
            let file = private::open(&file, false)?;
            let mut bytes = Vec::new();
            file.take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| hagency_store::Error::OutcomeUnknown)?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err(hagency_store::Error::Capacity);
            }
            let plan = serde_json::from_slice(&bytes)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| hagency_store::Error::OutcomeUnknown)?
                .as_millis();
            let now = u64::try_from(now).map_err(|_| hagency_store::Error::Capacity)?;
            domain.adopt_legacy_allocations(&plan, now)
        }
    }
}
