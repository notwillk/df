use std::io::{self, Write};

use anyhow::{Context, Result, bail};

use crate::config::ConfigStore;
use crate::state::DofPaths;
use crate::workspace;

pub(crate) fn list(json: bool) -> Result<()> {
    let paths = DofPaths::from_env()?;
    let config = ConfigStore::new(&paths).read()?;
    let features = workspace::discover_features(paths.workspace())?
        .into_iter()
        .filter(|feature| config.feature_enabled(&feature.name))
        .map(|feature| feature.name)
        .collect::<Vec<_>>();

    let stdout = io::stdout();
    let mut output = stdout.lock();

    if json {
        serde_json::to_writer(&mut output, &features)
            .context("failed to write feature list as JSON")?;
        writeln!(output).context("failed to write feature list")?;
    } else {
        for feature in features {
            writeln!(output, "{feature}").context("failed to write feature list")?;
        }
    }

    Ok(())
}

pub(crate) fn set_enabled(name: &str, enabled: bool) -> Result<()> {
    let paths = DofPaths::from_env()?;
    ConfigStore::new(&paths).update(|config| {
        let exists = workspace::discover_features(paths.workspace())?
            .into_iter()
            .any(|feature| feature.name == name);
        if !exists {
            bail!("feature {name:?} does not exist in workspace");
        }
        config.set_feature_enabled(name, enabled);
        Ok(())
    })
}
