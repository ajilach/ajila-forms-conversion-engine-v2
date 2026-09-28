//! `<binary> warm`: boot a profile's base image, wait until it reports
//! ready, stop it cleanly and commit the result so a later `verify_run`
//! need not pay for a cold boot. Not an MCP tool -- this runs once, ahead
//! of time, as an operator action (`just aem-warm`), not something an
//! agent triggers. See `crate::server::run_main` for where `warm` is
//! dispatched from each binary's own `main`.

use crate::profile::Profile;

/// The image tag [`warm_command`] commits to and
/// `crate::session::select_aem_image`'s image choice both check for. One
/// function so the name cannot drift between the writer and the two
/// readers.
pub fn warm_image_tag(format: &str) -> String {
    format!("u2s-aem-verify/{format}:warm")
}

/// Refuses outright for a `U2S_AEM_VERIFY_DATA_VOLUME` profile: `docker
/// commit` cannot capture a `VOLUME`-declared path's contents (only a
/// container's own layer), so committing here would silently produce an
/// image missing the entire JCR repository -- confirmed live against
/// `ajila.azurecr.io/aemforms-arm` (the committed image crashed on boot,
/// missing its own quickstart jar). That profile's fast-reboot benefit
/// already comes from the volume itself; `crate::session::boot` never
/// looks for a warm image when one is configured, so running this command
/// against it would build an artifact nothing ever reads, that would also
/// break if it were.
///
/// `program` names the calling binary purely for the messages this prints
/// -- `u2s-aem-verify-mcp` or `u2s-aem-ubs-verify-mcp`, whichever invoked
/// it -- so the operator sees which one they ran, not this shared crate's
/// own name.
pub async fn warm_command(profile: &Profile, program: &str) -> Result<(), String> {
    if let Some(volume) = &profile.aem_data_volume {
        return Err(format!(
            "this profile persists AEM state in the Docker volume {volume:?} \
             (U2S_AEM_VERIFY_DATA_VOLUME); committing a container as a warm image cannot \
             capture that state and would produce a broken image -- see docker/aem/README.md. \
             Nothing to do: the volume already gives verify_run a fast reboot."
        ));
    }

    let docker = u2s_verify_core::docker::DockerLifecycle::connect()
        .await
        .map_err(|err| format!("cannot reach Docker: {err}"))?;

    docker
        .ensure_image(&profile.aem_image, &profile.platform)
        .await
        .map_err(|err| err.to_string())?;
    let base_image_id = docker
        .image_id(&profile.aem_image)
        .await
        .map_err(|err| err.to_string())?
        .ok_or_else(|| format!("{} was just pulled but is not found", profile.aem_image))?;

    let network = format!("u2s-aem-verify-warm-{}", uuid::Uuid::new_v4().simple());
    docker
        .ensure_network(&network)
        .await
        .map_err(|err| err.to_string())?;

    let mut labels = profile.owner_labels();
    labels.insert("u2s.verify".to_owned(), "1".to_owned());
    labels.insert("u2s.verify.warm_build".to_owned(), "1".to_owned());

    let container = docker
        .run(&u2s_verify_core::docker::ContainerSpec {
            name: format!("u2s-aem-verify-warm-{}", uuid::Uuid::new_v4().simple()),
            image: profile.aem_image.clone(),
            platform: profile.platform.clone(),
            network: network.clone(),
            env: Vec::new(),
            labels,
            publish_ports: vec![profile.aem_container_port],
            binds: Vec::new(),
            extra_hosts: Vec::new(),
            memory_bytes: None,
        })
        .await
        .map_err(|err| err.to_string())?;

    println!(
        "{program} warm: {} started, waiting for it to be ready (up to {:?})",
        container.id, profile.boot_timeout
    );

    let port = container
        .published_port(profile.aem_container_port)
        .ok_or_else(|| {
            format!(
                "container {} never published port {}",
                container.id, profile.aem_container_port
            )
        })?;
    let ready = u2s_verify_core::docker::wait_for_http(
        &format!("http://127.0.0.1:{port}/libs/granite/core/content/login.html"),
        200,
        profile.boot_timeout,
        std::time::Duration::from_secs(3),
        None,
    )
    .await;

    if let Err(err) = ready {
        let _ = docker.teardown(&container.id).await;
        let _ = docker.remove_network(&network).await;
        return Err(err.to_string());
    }

    println!("{program} warm: ready, stopping and committing");

    // Stop without removing: a commit reads the container's current state,
    // so it must still exist when `commit` runs. `teardown` (stop *and*
    // remove) only happens after.
    docker
        .stop(&container.id)
        .await
        .map_err(|err| err.to_string())?;

    let tag = warm_image_tag(&profile.format);
    let mut image_labels = std::collections::HashMap::new();
    image_labels.insert("u2s.base_image_id".to_owned(), base_image_id);
    let commit_result = docker.commit(&container.id, &tag, image_labels).await;

    let teardown_result = docker.teardown(&container.id).await;
    let _ = docker.remove_network(&network).await;

    let image_id = commit_result.map_err(|err| err.to_string())?;
    if let Err(err) = teardown_result {
        eprintln!(
            "{program} warm: committed as {tag} ({image_id}), but could not remove the build \
             container: {err}"
        );
    } else {
        println!("{program} warm: committed as {tag} ({image_id})");
    }
    Ok(())
}
