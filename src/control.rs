use crate::error::*;
use crate::listener::Listener;
use crate::manifest::Config;
use crate::pathbytes::*;
use crate::tararchive::Archive;
use crate::wordsplit::WordSplit;
use crate::dh_installsystemd;
use crate::dh_lib;
use crate::util::find_first;
use md5::Digest;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Generates an uncompressed tar archive with `control`, `md5sums`, and others
pub fn generate_archive(options: &Config, time: u64, asset_hashes: HashMap<PathBuf, Digest>, listener: &mut dyn Listener) -> CDResult<Vec<u8>> {
    let mut archive = Archive::new(time);
    generate_md5sums(&mut archive, options, asset_hashes)?;
    generate_control(&mut archive, options, listener)?;
    if let Some(ref files) = options.conf_files {
        generate_conf_files(&mut archive, files)?;
    }
    generate_scripts(&mut archive, options, listener)?;
    if let Some(ref file) = options.triggers_file {
        generate_triggers_file(&mut archive, file)?;
    }
    Ok(archive.into_inner()?)
}

/// Append Debian maintainer script files (control, preinst, postinst, prerm,
/// postrm and templates) present in the `maintainer_scripts` path to the
/// archive, if `maintainer_scripts` is configured.
/// 
/// Additionally, when `systemd_units` is configured, shell script fragments
/// "for enabling, disabling, starting, stopping and restarting systemd unit
/// files" (quoting man 1 dh_installsystemd) will replace the `#DEBHELPER#`
/// token in the provided maintainer scripts.
/// 
/// If a shell fragment cannot be inserted because the target script is missing
/// then the entire script will be generated and appended to the archive.
/// 
/// # Requirements
/// 
/// When `systemd_units` is configured, user supplied `maintainer_scripts` must
/// contain a `#DEBHELPER#` token at the point where shell script fragments
/// should be inserted.
/// 
/// When `systemd_units` is configured, a directory for temporarily storing
/// selected shell script fragments will be created inside the cargo deb temp
/// directory, and will be cleaned up/removed when the cargo deb temp directory
/// is cleaned up/removed.
/// 
/// # Panics
/// 
/// When `systemd_units` is configured, failure to replace the `#DEBHELPER#`
/// token because it is not present in a user supplied `maintainer_script` will
/// cause a panic.
fn generate_scripts(archive: &mut Archive, option: &Config, listener: &mut dyn Listener) -> CDResult<()> {
    if let Some(ref maintainer_scripts) = option.maintainer_scripts {
        let mut search_dirs = vec![maintainer_scripts.clone()];

        if let Some(systemd_units_config) = &option.systemd_units {
            // Ensure we have a clean temporary directory for working on
            // maintainer scripts and autoscript fragments.
            let tmp_dir = option.deb_temp_dir().join("systemd");
            if tmp_dir.exists() {
                fs::remove_dir_all(&tmp_dir).map_err(|e| CargoDebError::Io(e))?;
            }
            fs::create_dir(&tmp_dir).map_err(|e| CargoDebError::Io(e))?;

            // Select and populate autoscript templates relevant to the unit
            // file(s) in this package and the configuration settings chosen.
            dh_installsystemd::generate(
                maintainer_scripts,
                &option.name,
                &option.assets.resolved,
                &tmp_dir,
                &dh_installsystemd::Options::from(systemd_units_config),
                listener);
                
            // Get Option<&str> from Option<String>
            let unit_name = systemd_units_config.unit_name
                .as_ref().map(|s| s.as_str());

            // Replace the #DEBHELPER# token in the users maintainer scripts
            // and/or generate maintainer scripts from scratch as needed.
            dh_lib::apply(
                &tmp_dir,
                &option.name,
                unit_name,
                listener);

            // Use the maintainer scripts that we just created/customized in
            // preference to the unmodified user supplied versions.
            search_dirs.insert(0, tmp_dir);
        }

        // Add maintainer scripts to the archive, either those supplied by the
        // user or if available prefer modified versions generated above.
        for name in &["config", "preinst", "postinst", "prerm", "postrm", "templates"] {
            if let Some(script_path) = find_first(search_dirs.as_slice(), name) {
                let abs_path = script_path.canonicalize().unwrap();
                let rel_path = abs_path.strip_prefix(&option.manifest_dir).unwrap();
                listener.info(format!("Archiving {}", rel_path.to_string_lossy()));

                if let Ok(script) = fs::read(script_path) {
                    archive.file(name, &script, 0o755)?;
                }
            }
        }
    }

    Ok(())
}

/// Creates the md5sums file which contains a list of all contained files and the md5sums of each.
fn generate_md5sums(archive: &mut Archive, options: &Config, asset_hashes: HashMap<PathBuf, Digest>) -> CDResult<()> {
    let mut md5sums: Vec<u8> = Vec::new();

    // Collect md5sums from each asset in the archive (excludes symlinks).
    for asset in &options.assets.resolved {
        if let Some(value) = asset_hashes.get(&asset.target_path) {
            write!(md5sums, "{:x}", value)?;
            md5sums.write_all(b"  ")?;

            md5sums.write_all(&asset.target_path.as_path().as_unix_path())?;
            md5sums.write_all(&[b'\n'])?;
        }
    }

    // Write the data to the archive
    archive.file("./md5sums", &md5sums, 0o644)?;
    Ok(())
}

/// Generates the control file that obtains all the important information about the package.
fn generate_control(archive: &mut Archive, options: &Config, listener: &mut dyn Listener) -> CDResult<()> {
    // Create and return the handle to the control file with write access.
    let mut control: Vec<u8> = Vec::with_capacity(1024);

    // Write all of the lines required by the control file.
    writeln!(&mut control, "Package: {}", options.deb_name)?;
    writeln!(&mut control, "Version: {}", options.deb_version)?;
    writeln!(&mut control, "Architecture: {}", options.architecture)?;
    if let Some(ref repo) = options.repository {
        if repo.starts_with("http") {
            writeln!(&mut control, "Vcs-Browser: {}", repo)?;
        }
        if let Some(kind) = options.repository_type() {
            writeln!(&mut control, "Vcs-{}: {}", kind, repo)?;
        }
    }
    if let Some(homepage) = options.homepage.as_ref().or(options.documentation.as_ref()) {
        writeln!(&mut control, "Homepage: {}", homepage)?;
    }
    if let Some(ref section) = options.section {
        writeln!(&mut control, "Section: {}", section)?;
    }
    writeln!(&mut control, "Priority: {}", options.priority)?;
    control.write_all(b"Standards-Version: 3.9.4\n")?;
    writeln!(&mut control, "Maintainer: {}", options.maintainer)?;

    let installed_size = options.assets.resolved
        .iter()
        .filter_map(|m| m.source.len())
        .sum::<u64>() / 1024;

    writeln!(&mut control, "Installed-Size: {}", installed_size)?;

    writeln!(&mut control, "Depends: {}", options.get_dependencies(listener)?)?;

    if let Some(ref build_depends) = options.build_depends {
        writeln!(&mut control, "Build-Depends: {}", build_depends)?;
    }

    if let Some(ref conflicts) = options.conflicts {
        writeln!(&mut control, "Conflicts: {}", conflicts)?;
    }
    if let Some(ref breaks) = options.breaks {
        writeln!(&mut control, "Breaks: {}", breaks)?;
    }
    if let Some(ref replaces) = options.replaces {
        writeln!(&mut control, "Replaces: {}", replaces)?;
    }
    if let Some(ref provides) = options.provides {
        writeln!(&mut control, "Provides: {}", provides)?;
    }

    write!(&mut control, "Description:")?;
    for line in options.description.split_by_chars(79) {
        writeln!(&mut control, " {}", line)?;
    }

    if let Some(ref desc) = options.extended_description {
        for line in desc.split_by_chars(79) {
            writeln!(&mut control, " {}", line)?;
        }
    }
    control.push(10);

    // Add the control file to the tar archive.
    archive.file("./control", &control, 0o644)?;
    Ok(())
}

/// If configuration files are required, the conffiles file will be created.
fn generate_conf_files(archive: &mut Archive, files: &str) -> CDResult<()> {
    let mut data = Vec::new();
    data.write_all(files.as_bytes())?;
    data.push(b'\n');
    archive.file("./conffiles", &data, 0o644)?;
    Ok(())
}

fn generate_triggers_file<P: AsRef<Path>>(archive: &mut Archive, path: P) -> CDResult<()> {
    if let Ok(content) = fs::read(path) {
        archive.file("./triggers", &content, 0o644)?;
    }
    Ok(())
}
