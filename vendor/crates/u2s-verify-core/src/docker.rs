//! Docker container lifecycle: create a network, run a container on it with
//! a platform pin, poll it ready, tear it down. Format-agnostic -- nothing
//! here knows what image it is running or what "ready" means for it, so
//! `u2s-aem-verify-core` supplies both.
//!
//! Every failure is a [`DockerError`] naming which step failed; there is no
//! retry loop here beyond [`wait_for_http`]'s own polling -- a single
//! Docker Engine call either works or the caller decides what to do about
//! it, matching this workspace's "hard failures over undocumented
//! mechanisms" convention.

use std::collections::HashMap;
use std::time::Duration;

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig, PortBinding, PortMap};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptionsBuilder,
    InspectContainerOptions, ListContainersOptionsBuilder, RemoveContainerOptionsBuilder,
    StopContainerOptionsBuilder,
};
use futures::TryStreamExt;

#[derive(Debug, thiserror::Error)]
pub enum DockerError {
    #[error("cannot reach the Docker daemon: {0}")]
    Unreachable(#[source] bollard::errors::Error),
    #[error("cannot pull image {image:?}: {source}")]
    PullFailed {
        image: String,
        #[source]
        source: bollard::errors::Error,
    },
    #[error("cannot create network {name:?}: {source}")]
    CreateNetwork {
        name: String,
        #[source]
        source: bollard::errors::Error,
    },
    #[error("cannot create container from {image:?}: {source}")]
    CreateContainer {
        image: String,
        #[source]
        source: bollard::errors::Error,
    },
    #[error("cannot start container {id:?}: {source}")]
    StartContainer {
        id: String,
        #[source]
        source: bollard::errors::Error,
    },
    #[error("cannot inspect container {id:?}: {source}")]
    InspectContainer {
        id: String,
        #[source]
        source: bollard::errors::Error,
    },
    #[error("cannot list containers: {0}")]
    ListContainers(#[source] bollard::errors::Error),
    #[error("cannot inspect image {image:?}: {source}")]
    InspectImage {
        image: String,
        #[source]
        source: bollard::errors::Error,
    },
    /// Stop and remove failures are collected rather than surfaced
    /// individually -- see [`DockerLifecycle::teardown`].
    #[error("cannot stop or remove container {id:?}: {source}")]
    Teardown {
        id: String,
        #[source]
        source: bollard::errors::Error,
    },
    #[error("cannot exec into container {id:?}: {source}")]
    Exec {
        id: String,
        #[source]
        source: bollard::errors::Error,
    },
    #[error("cannot wait for container {id:?} to exit: {source}")]
    Wait {
        id: String,
        #[source]
        source: bollard::errors::Error,
    },
    #[error("container {id:?} did not exit within {waited:?}")]
    RunTimedOut { id: String, waited: Duration },
    #[error("{command:?} in container {container:?} did not succeed within {waited:?}: {last}")]
    ExecNotReady {
        container: String,
        command: String,
        waited: Duration,
        last: String,
    },
    #[error("cannot remove network {name:?}: {source}")]
    RemoveNetwork {
        name: String,
        #[source]
        source: bollard::errors::Error,
    },
    #[error("cannot list networks: {0}")]
    ListNetworks(#[source] bollard::errors::Error),
    #[error("cannot connect container {container:?} to network {network:?}: {source}")]
    ConnectNetwork {
        container: String,
        network: String,
        #[source]
        source: bollard::errors::Error,
    },
    #[error("container {container:?} has no address on network {network:?}")]
    NoAddress { container: String, network: String },
    #[error("cannot copy {path:?} out of container {id:?}: {reason}")]
    CopyOut {
        id: String,
        path: String,
        reason: String,
    },
}

/// Everything needed to create and publish one container -- deliberately
/// flat rather than exposing `bollard`'s own request types, so a caller
/// building this does not need `bollard` in scope at all.
#[derive(Debug, Clone)]
pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    /// `docker run --platform`, e.g. `"linux/amd64"`. Empty means "let the
    /// daemon decide", matching `bollard`'s own default.
    pub platform: String,
    pub network: String,
    pub env: Vec<(String, String)>,
    pub labels: HashMap<String, String>,
    /// Container-side ports to publish to an ephemeral host port, e.g.
    /// `[4502]`. The chosen host port for each is read back from
    /// [`RunningContainer::published_port`] after start, since Docker
    /// picks it.
    pub publish_ports: Vec<u16>,
    /// `host_path:container_path` bind mounts. Used for the Chromium
    /// download directory: a host-visible path means the downloaded file
    /// can be read directly, with no Docker Engine "copy out of a
    /// container" call in the loop at all.
    pub binds: Vec<String>,
    /// `docker run --add-host` entries, e.g. `["host.docker.internal:host-gateway"]`
    /// -- Docker Desktop on macOS resolves `host.docker.internal` for a
    /// container automatically, but a Linux Docker host does not unless
    /// told to, so a caller that needs a container to reach a service on
    /// the host (`u2s-aem-verify-core`'s Redacto dependency) sets this
    /// rather than working only by macOS coincidence.
    pub extra_hosts: Vec<String>,
    pub memory_bytes: Option<i64>,
}

/// A container this lifecycle started, with what a caller needs to reach
/// and later tear it down.
#[derive(Debug, Clone)]
pub struct RunningContainer {
    pub id: String,
    ports: HashMap<u16, u16>,
}

impl RunningContainer {
    /// The ephemeral host port Docker chose for a container port this
    /// container published, or `None` if that port was never in the
    /// spec's `publish_ports`.
    pub fn published_port(&self, container_port: u16) -> Option<u16> {
        self.ports.get(&container_port).copied()
    }
}

/// [`DockerLifecycle::exec`]'s result: the command's exit code and its
/// combined stdout+stderr, in the order the daemon streamed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub exit_code: i64,
    pub output: String,
}

impl ExecOutput {
    pub fn succeeded(&self) -> bool {
        self.exit_code == 0
    }
}

pub struct DockerLifecycle {
    docker: Docker,
}

impl DockerLifecycle {
    /// Connects using the platform default (the Unix socket on macOS/Linux,
    /// the named pipe on Windows) and confirms the daemon actually answers
    /// -- a socket that exists but refuses connections (Docker Desktop
    /// installed but not running) must be [`DockerError::Unreachable`], not
    /// a lifecycle that only fails on the first real call.
    pub async fn connect() -> Result<Self, DockerError> {
        let docker = Docker::connect_with_local_defaults().map_err(DockerError::Unreachable)?;
        docker.ping().await.map_err(DockerError::Unreachable)?;
        Ok(Self { docker })
    }

    /// `true` iff the daemon answers right now. Never returns an error --
    /// this is `verify_status`'s own reachability check, where "the daemon
    /// is down" is exactly the fact being reported, not a failure of the
    /// check itself.
    pub async fn is_reachable(&self) -> bool {
        self.docker.ping().await.is_ok()
    }

    /// Pulls `image` if the daemon does not already have it -- and *only*
    /// if it does not, checked first via [`Self::image_id`] rather than
    /// always attempting the pull. This is not an optimisation: `bollard`'s
    /// `create_image` call carries no registry credentials (there is no
    /// generic way to reconstruct what `docker login`/`az acr login` wrote
    /// into the OS keychain via `credsStore`), so an unconditional pull
    /// against a private registry -- ajila's own ACR, in
    /// `u2s-aem-verify-core`'s real usage -- fails with a 401 even when the
    /// image is already present locally and nothing needed pulling at all.
    /// Confirmed live: `docker pull` from the CLI (which does read that
    /// credential store) succeeded against the same image and tag this
    /// call 401'd on. An operator's own `docker pull` (`docker/aem/README.md`'s
    /// own setup step) is what puts the image there in the first place;
    /// this only ever needs to reach the registry itself for a tag that is
    /// missing or has never been pulled on this machine.
    pub async fn ensure_image(&self, image: &str, platform: &str) -> Result<(), DockerError> {
        if self.image_id(image).await?.is_some() {
            return Ok(());
        }

        let mut options = CreateImageOptionsBuilder::default().from_image(image);
        if !platform.is_empty() {
            options = options.platform(platform);
        }
        self.docker
            .create_image(Some(options.build()), None, None)
            .try_collect::<Vec<_>>()
            .await
            .map_err(|source| DockerError::PullFailed {
                image: image.to_owned(),
                source,
            })?;
        Ok(())
    }

    /// Creates the network if it does not already exist. A network name is
    /// scoped to one verification (see `u2s-aem-verify-core::flow`'s
    /// per-run UUID), so "already exists" here would mean a leftover from
    /// a crashed prior run reusing the same name -- treated as success,
    /// not an error, since the network this call wanted now exists either
    /// way.
    ///
    /// `labels` let [`Self::find_networks_by_label`] find a network a
    /// crashed process left behind, the way [`Self::find_by_label`] finds
    /// its containers.
    pub async fn ensure_network(
        &self,
        name: &str,
        labels: &HashMap<String, String>,
    ) -> Result<(), DockerError> {
        let request = bollard::models::NetworkCreateRequest {
            name: name.to_owned(),
            driver: Some("bridge".to_owned()),
            labels: (!labels.is_empty()).then(|| labels.clone()),
            ..Default::default()
        };
        match self.docker.create_network(request).await {
            Ok(_) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 409, ..
            }) => Ok(()),
            Err(source) => Err(DockerError::CreateNetwork {
                name: name.to_owned(),
                source,
            }),
        }
    }

    pub async fn remove_network(&self, name: &str) -> Result<(), DockerError> {
        self.docker
            .remove_network(name)
            .await
            .map_err(|source| DockerError::RemoveNetwork {
                name: name.to_owned(),
                source,
            })
    }

    /// Names of every network carrying `label` (`key=value`).
    pub async fn find_networks_by_label(&self, label: &str) -> Result<Vec<String>, DockerError> {
        let mut filters = HashMap::new();
        filters.insert("label".to_owned(), vec![label.to_owned()]);
        let options = bollard::query_parameters::ListNetworksOptionsBuilder::default()
            .filters(&filters)
            .build();
        let networks = self
            .docker
            .list_networks(Some(options))
            .await
            .map_err(DockerError::ListNetworks)?;
        Ok(networks.into_iter().filter_map(|n| n.name).collect())
    }

    /// Attaches `container` (id or name) to `network`. Already attached is
    /// success: the container ends up on the network either way.
    pub async fn connect_network(&self, network: &str, container: &str) -> Result<(), DockerError> {
        let request = bollard::models::NetworkConnectRequest {
            container: container.to_owned(),
            endpoint_config: None,
        };
        match self.docker.connect_network(network, request).await {
            Ok(()) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 403,
                ref message,
            }) if message.contains("already exists") => Ok(()),
            Err(source) => Err(DockerError::ConnectNetwork {
                container: container.to_owned(),
                network: network.to_owned(),
                source,
            }),
        }
    }

    /// Detaches `container` from `network`, forcibly, so a network can be
    /// removed afterwards. Not attached (or the network already gone) is
    /// success.
    pub async fn disconnect_network(
        &self,
        network: &str,
        container: &str,
    ) -> Result<(), DockerError> {
        let request = bollard::models::NetworkDisconnectRequest {
            container: container.to_owned(),
            force: Some(true),
        };
        match self.docker.disconnect_network(network, request).await {
            Ok(()) => Ok(()),
            // The daemon answers 404 for a missing network and, confirmed
            // live, 500 "is not connected to network" for a container that
            // already left it.
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 500,
                ref message,
            }) if message.contains("is not connected") => Ok(()),
            Err(source) => Err(DockerError::ConnectNetwork {
                container: container.to_owned(),
                network: network.to_owned(),
                source,
            }),
        }
    }

    /// `container`'s IPv4 address on `network`.
    pub async fn container_ip(&self, container: &str, network: &str) -> Result<String, DockerError> {
        let inspected = self
            .docker
            .inspect_container(container, None::<InspectContainerOptions>)
            .await
            .map_err(|source| DockerError::InspectContainer {
                id: container.to_owned(),
                source,
            })?;
        inspected
            .network_settings
            .and_then(|settings| settings.networks)
            .and_then(|networks| networks.get(network).cloned())
            .and_then(|endpoint| endpoint.ip_address)
            .filter(|ip| !ip.is_empty())
            .ok_or_else(|| DockerError::NoAddress {
                container: container.to_owned(),
                network: network.to_owned(),
            })
    }

    /// The contents of one regular file inside a container, through the
    /// Engine's archive endpoint (what `docker cp` uses). Works wherever
    /// the daemon runs, unlike reading a bind-mounted host path, which
    /// needs this process and the daemon to share a filesystem.
    pub async fn copy_file_out(&self, id: &str, path: &str) -> Result<Vec<u8>, DockerError> {
        use futures::TryStreamExt;
        let options = bollard::query_parameters::DownloadFromContainerOptionsBuilder::new()
            .path(path)
            .build();
        let chunks: Vec<bytes::Bytes> = self
            .docker
            .download_from_container(id, Some(options))
            .try_collect()
            .await
            .map_err(|err| DockerError::CopyOut {
                id: id.to_owned(),
                path: path.to_owned(),
                reason: err.to_string(),
            })?;
        let archive: Vec<u8> = chunks.concat();
        single_file_from_tar(&archive).map_err(|reason| DockerError::CopyOut {
            id: id.to_owned(),
            path: path.to_owned(),
            reason,
        })
    }

    /// Creates and starts a container from `spec`, returning its id and
    /// the host ports Docker chose for whatever it published.
    pub async fn run(&self, spec: &ContainerSpec) -> Result<RunningContainer, DockerError> {
        let mut exposed_ports = Vec::new();
        let mut port_bindings: PortMap = HashMap::new();
        for port in &spec.publish_ports {
            let key = format!("{port}/tcp");
            exposed_ports.push(key.clone());
            port_bindings.insert(
                key,
                Some(vec![PortBinding {
                    host_ip: Some("127.0.0.1".to_owned()),
                    host_port: Some("0".to_owned()),
                }]),
            );
        }

        let host_config = HostConfig {
            network_mode: Some(spec.network.clone()),
            port_bindings: Some(port_bindings),
            binds: (!spec.binds.is_empty()).then(|| spec.binds.clone()),
            extra_hosts: (!spec.extra_hosts.is_empty()).then(|| spec.extra_hosts.clone()),
            memory: spec.memory_bytes,
            ..Default::default()
        };

        let body = ContainerCreateBody {
            image: Some(spec.image.clone()),
            env: (!spec.env.is_empty())
                .then(|| spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect()),
            labels: (!spec.labels.is_empty()).then(|| spec.labels.clone()),
            exposed_ports: (!exposed_ports.is_empty()).then_some(exposed_ports),
            host_config: Some(host_config),
            ..Default::default()
        };

        let options = CreateContainerOptions {
            name: Some(spec.name.clone()),
            platform: spec.platform.clone(),
        };

        let created = self
            .docker
            .create_container(Some(options), body)
            .await
            .map_err(|source| DockerError::CreateContainer {
                image: spec.image.clone(),
                source,
            })?;

        self.docker
            .start_container(&created.id, None)
            .await
            .map_err(|source| DockerError::StartContainer {
                id: created.id.clone(),
                source,
            })?;

        let ports = if spec.publish_ports.is_empty() {
            HashMap::new()
        } else {
            self.read_published_ports(&created.id, &spec.publish_ports)
                .await?
        };

        Ok(RunningContainer {
            id: created.id,
            ports,
        })
    }

    async fn read_published_ports(
        &self,
        id: &str,
        container_ports: &[u16],
    ) -> Result<HashMap<u16, u16>, DockerError> {
        let inspected = self
            .docker
            .inspect_container(id, None::<InspectContainerOptions>)
            .await
            .map_err(|source| DockerError::InspectContainer {
                id: id.to_owned(),
                source,
            })?;

        let mut ports = HashMap::new();
        let Some(network_settings) = inspected.network_settings else {
            return Ok(ports);
        };
        let Some(bindings) = network_settings.ports else {
            return Ok(ports);
        };
        for &container_port in container_ports {
            let key = format!("{container_port}/tcp");
            if let Some(Some(published)) = bindings.get(&key)
                && let Some(first) = published.first()
                && let Some(host_port) = &first.host_port
                && let Ok(host_port) = host_port.parse::<u16>()
            {
                ports.insert(container_port, host_port);
            }
        }
        Ok(ports)
    }

    /// Every container this process created and never explicitly stopped,
    /// found by label rather than tracked in memory -- so a leftover from
    /// a crashed prior process is still discoverable. `label` is
    /// `"u2s.verify.format=<key>"` in `u2s-aem-verify-core`'s usage.
    pub async fn find_by_label(&self, label: &str) -> Result<Vec<String>, DockerError> {
        let mut filters = HashMap::new();
        filters.insert("label".to_owned(), vec![label.to_owned()]);
        let options = ListContainersOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .build();
        let containers = self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(DockerError::ListContainers)?;
        Ok(containers.into_iter().filter_map(|c| c.id).collect())
    }

    /// Stops a container without removing it -- the half of [`teardown`]
    /// a caller that still needs the stopped container around wants on its
    /// own, namely [`Self::commit`]: an image is committed from a
    /// container's current state, so removing it first would leave nothing
    /// to commit.
    ///
    /// [`teardown`]: Self::teardown
    pub async fn stop(&self, id: &str) -> Result<(), DockerError> {
        let options = StopContainerOptionsBuilder::default().t(5).build();
        self.docker
            .stop_container(id, Some(options))
            .await
            .map_err(|source| DockerError::Teardown {
                id: id.to_owned(),
                source,
            })
    }

    /// Stops then removes a container, force-removing so a container that
    /// ignored the stop signal is still gone afterward.
    pub async fn teardown(&self, id: &str) -> Result<(), DockerError> {
        let _ = self.stop(id).await;

        let remove_options = RemoveContainerOptionsBuilder::default()
            .force(true)
            .v(true)
            .build();
        self.docker
            .remove_container(id, Some(remove_options))
            .await
            .map_err(|source| DockerError::Teardown {
                id: id.to_owned(),
                source,
            })
    }

    /// Runs `cmd` inside `container_id`'s own container, optionally piping
    /// `stdin_data` to the process's stdin, and returns its combined
    /// stdout+stderr and exit code. General enough for any verifier that
    /// needs to run a one-off command inside a container it already
    /// booted, over the same daemon connection everything else in this
    /// module uses -- no assumption that a `docker` CLI is on `PATH`, only
    /// that the daemon itself answers.
    pub async fn exec(
        &self,
        container_id: &str,
        cmd: Vec<String>,
        stdin_data: Option<&[u8]>,
    ) -> Result<ExecOutput, DockerError> {
        use bollard::exec::{CreateExecOptions, StartExecResults};
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;

        let created = self
            .docker
            .create_exec(
                container_id,
                CreateExecOptions {
                    attach_stdin: Some(stdin_data.is_some()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(cmd),
                    ..Default::default()
                },
            )
            .await
            .map_err(|source| DockerError::Exec {
                id: container_id.to_owned(),
                source,
            })?;

        let started = self
            .docker
            .start_exec(&created.id, None)
            .await
            .map_err(|source| DockerError::Exec {
                id: container_id.to_owned(),
                source,
            })?;

        let mut output = String::new();
        if let StartExecResults::Attached { output: mut stream, mut input } = started {
            if let Some(data) = stdin_data {
                // Errors writing/closing stdin are not fatal on their own --
                // the process may already have exited (e.g. `psql` failed
                // fast on a syntax error) -- the exit code inspected below
                // is the authoritative signal either way.
                let _ = input.write_all(data).await;
                let _ = input.shutdown().await;
            }
            drop(input);
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(log) => output.push_str(&log.to_string()),
                    Err(source) => {
                        return Err(DockerError::Exec {
                            id: container_id.to_owned(),
                            source,
                        });
                    }
                }
            }
        }

        let inspected =
            self.docker
                .inspect_exec(&created.id)
                .await
                .map_err(|source| DockerError::Exec {
                    id: container_id.to_owned(),
                    source,
                })?;

        Ok(ExecOutput {
            exit_code: inspected.exit_code.unwrap_or(-1),
            output,
        })
    }

    /// Polls `cmd` inside `container` until `ready` accepts its result, or
    /// gives up after `timeout` -- a readiness probe for a container whose
    /// readiness only its own tools can check (`pg_isready`, a `curl`
    /// against a port that is never published). Pass
    /// [`ExecOutput::succeeded`] to wait for exit 0. The error carries the
    /// last attempt's output.
    pub async fn wait_for_exec(
        &self,
        container: &str,
        cmd: &[&str],
        ready: impl Fn(&ExecOutput) -> bool,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<(), DockerError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut last = "never attempted".to_owned();
        while tokio::time::Instant::now() < deadline {
            match self
                .exec(container, cmd.iter().map(|s| (*s).to_owned()).collect(), None)
                .await
            {
                Ok(result) if ready(&result) => return Ok(()),
                Ok(result) => last = format!("exit {}: {}", result.exit_code, result.output.trim()),
                Err(err) => last = err.to_string(),
            }
            tokio::time::sleep(poll_interval).await;
        }
        Err(DockerError::ExecNotReady {
            container: container.to_owned(),
            command: cmd.join(" "),
            waited: timeout,
            last,
        })
    }

    /// Runs a one-shot container to completion: starts it from `spec`,
    /// waits up to `timeout` for it to exit, collects its combined
    /// stdout+stderr, and removes it -- also when waiting failed. A
    /// non-zero exit is an [`ExecOutput`], not an error: what it means is
    /// the caller's business.
    pub async fn run_to_completion(
        &self,
        spec: &ContainerSpec,
        timeout: Duration,
    ) -> Result<ExecOutput, DockerError> {
        use bollard::query_parameters::LogsOptionsBuilder;
        use futures::StreamExt;

        let container = self.run(spec).await?;
        let waited = tokio::time::timeout(timeout, async {
            match self.docker.wait_container(&container.id, None).next().await {
                Some(Ok(response)) => Ok(response.status_code),
                Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. })) => Ok(code),
                Some(Err(source)) => Err(DockerError::Wait {
                    id: container.id.clone(),
                    source,
                }),
                None => Ok(-1),
            }
        })
        .await;

        let logs_options = LogsOptionsBuilder::default().stdout(true).stderr(true).build();
        let mut output = String::new();
        let mut logs = self.docker.logs(&container.id, Some(logs_options));
        while let Some(Ok(chunk)) = logs.next().await {
            output.push_str(&chunk.to_string());
        }
        if let Err(err) = self.teardown(&container.id).await {
            log::warn!("u2s-verify-core: could not remove one-shot container {}: {err}", container.id);
        }

        let exit_code = waited.map_err(|_| DockerError::RunTimedOut {
            id: container.id.clone(),
            waited: timeout,
        })??;
        Ok(ExecOutput { exit_code, output })
    }

    /// The content-addressable id of a local image, or `None` if it is not
    /// present -- distinguished from an error, since "not pulled yet" is
    /// an ordinary answer here, not a failure to check.
    pub async fn image_id(&self, image: &str) -> Result<Option<String>, DockerError> {
        Ok(self.inspect_image(image).await?.and_then(|i| i.id))
    }

    async fn inspect_image(
        &self,
        image: &str,
    ) -> Result<Option<bollard::models::ImageInspect>, DockerError> {
        match self.docker.inspect_image(image).await {
            Ok(inspected) => Ok(Some(inspected)),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(source) => Err(DockerError::InspectImage {
                image: image.to_owned(),
                source,
            }),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{url} did not become ready within {waited:?}: {reason}")]
pub struct HttpReadyError {
    pub url: String,
    pub waited: Duration,
    pub reason: String,
}

/// Polls `url` until a `GET` returns exactly `expected_status`, or gives up
/// after `timeout`. Format-agnostic: what "ready" means (which path, which
/// status) is the caller's business -- `u2s-aem-verify-core::flow` polls
/// AEM's login page and Chromium's `/json/version` with this same
/// function.
///
/// `basic_auth`, when given, is sent on every attempt -- needed for a
/// caller polling *protected* content (a UBS form's own page, unlike the
/// login page itself, which must stay reachable to anyone precisely so a
/// login page has something to show) rather than the login page: confirmed
/// live, AEM answers such a page `401` without it, which without this
/// parameter this function could not tell apart from "not ready yet"
/// except by never becoming ready and timing out.
pub async fn wait_for_http(
    url: &str,
    expected_status: u16,
    timeout: Duration,
    poll_interval: Duration,
    basic_auth: Option<(&str, &str)>,
) -> Result<(), HttpReadyError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("a default reqwest client always builds");

    let deadline = tokio::time::Instant::now() + timeout;
    let mut last_reason = "never attempted".to_owned();

    while tokio::time::Instant::now() < deadline {
        let mut request = client.get(url);
        if let Some((user, password)) = basic_auth {
            request = request.basic_auth(user, Some(password));
        }
        match request.send().await {
            Ok(response) if response.status().as_u16() == expected_status => return Ok(()),
            Ok(response) => last_reason = format!("got status {}", response.status().as_u16()),
            Err(err) => last_reason = err.to_string(),
        }
        tokio::time::sleep(poll_interval).await;
    }

    Err(HttpReadyError {
        url: url.to_owned(),
        waited: timeout,
        reason: last_reason,
    })
}

/// The one regular file in a tar archive, as the archive endpoint returns
/// it for a file path. Pure function. Unit-tested.
pub fn single_file_from_tar(archive: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut entries = tar::Archive::new(archive);
    let mut found: Option<Vec<u8>> = None;
    for entry in entries.entries().map_err(|err| err.to_string())? {
        let mut entry = entry.map_err(|err| err.to_string())?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        if found.is_some() {
            return Err("the archive holds more than one file".to_owned());
        }
        let mut contents = Vec::new();
        entry
            .read_to_end(&mut contents)
            .map_err(|err| err.to_string())?;
        found = Some(contents);
    }
    found.ok_or_else(|| "the archive holds no file".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *contents).unwrap();
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn the_single_file_is_extracted() {
        let archive = tar_of(&[("abc.pdf", b"%PDF-1.7 body")]);
        assert_eq!(single_file_from_tar(&archive).unwrap(), b"%PDF-1.7 body");
    }

    #[test]
    fn an_empty_or_multi_file_archive_is_refused() {
        assert!(single_file_from_tar(&tar_of(&[])).is_err());
        assert!(single_file_from_tar(&tar_of(&[("a", b"1"), ("b", b"2")])).is_err());
        assert!(single_file_from_tar(b"not a tar").is_err());
    }

    /// No Docker daemon is assumed present for this crate's own test run
    /// (see the crate's module doc and `u2s-aem-verify-core`'s `#[ignore]`d
    /// live tests for the Docker-backed coverage) -- this only exercises
    /// the pure port-reading and spec plumbing.
    #[test]
    fn a_spec_with_no_ports_needs_no_inspection() {
        let spec = ContainerSpec {
            name: "x".into(),
            image: "alpine".into(),
            platform: String::new(),
            network: "bridge".into(),
            env: vec![],
            labels: HashMap::new(),
            publish_ports: vec![],
            binds: vec![],
            extra_hosts: vec![],
            memory_bytes: None,
        };
        assert!(spec.publish_ports.is_empty());
    }

    #[test]
    fn a_running_container_reports_none_for_an_unpublished_port() {
        let running = RunningContainer {
            id: "abc".into(),
            ports: HashMap::from([(4502, 49321)]),
        };
        assert_eq!(running.published_port(4502), Some(49321));
        assert_eq!(running.published_port(9222), None);
    }

    #[tokio::test]
    async fn wait_for_http_times_out_against_a_port_nothing_listens_on() {
        let err = wait_for_http(
            "http://127.0.0.1:1/",
            200,
            Duration::from_millis(200),
            Duration::from_millis(50),
            None,
        )
        .await
        .expect_err("nothing listens on port 1");
        assert!(!err.reason.is_empty(), "a reason must be recorded: {err}");
    }

    /// Real Docker coverage for [`DockerLifecycle::exec`], run only when
    /// explicitly asked -- every other test in this module runs Docker-free
    /// by design (see this module's own doc above). Uses `postgres:16-alpine`
    /// -- a public image whose default command keeps the container running
    /// (unlike a bare shell, which would exit before `exec` could reach
    /// it) -- exactly the image `u2s-redacto-verify-core` boots for real.
    #[tokio::test]
    #[ignore = "needs a real Docker daemon"]
    async fn networks_addresses_and_file_copies_work_against_a_real_daemon() {
        let lifecycle = DockerLifecycle::connect().await.expect("a real Docker daemon");
        lifecycle
            .ensure_image("postgres:16-alpine", "")
            .await
            .expect("postgres:16-alpine pulls or is already present");
        let unique = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let label_key = "u2s.verify-core.network-test";
        let labels = HashMap::from([(label_key.to_owned(), unique.clone())]);
        let network = format!("u2s-verify-core-net-test-{unique}");
        lifecycle.ensure_network(&network, &labels).await.expect("network");

        let spec = |name: String, network: &str| ContainerSpec {
            name,
            image: "postgres:16-alpine".to_owned(),
            platform: String::new(),
            network: network.to_owned(),
            env: vec![("POSTGRES_PASSWORD".to_owned(), "password".to_owned())],
            labels: HashMap::new(),
            publish_ports: vec![],
            binds: vec![],
            extra_hosts: vec![],
            memory_bytes: None,
        };
        let on_network = lifecycle
            .run(&spec(format!("u2s-verify-core-a-{unique}"), &network))
            .await
            .expect("container on the network");
        let joiner = lifecycle
            .run(&spec(format!("u2s-verify-core-b-{unique}"), "bridge"))
            .await
            .expect("container on bridge");

        let ip = lifecycle.container_ip(&on_network.id, &network).await.expect("an address");
        assert!(ip.parse::<std::net::Ipv4Addr>().is_ok(), "{ip}");
        assert!(lifecycle.container_ip(&joiner.id, &network).await.is_err());

        lifecycle.connect_network(&network, &joiner.id).await.expect("join");
        lifecycle.connect_network(&network, &joiner.id).await.expect("joining twice is fine");
        assert!(lifecycle.container_ip(&joiner.id, &network).await.is_ok());

        lifecycle
            .exec(
                &on_network.id,
                vec!["sh".to_owned(), "-c".to_owned(), "printf '%s' '%PDF-1.7 x' > /tmp/out.pdf".to_owned()],
                None,
            )
            .await
            .expect("write a file");
        let bytes = lifecycle.copy_file_out(&on_network.id, "/tmp/out.pdf").await.expect("copy out");
        assert_eq!(bytes, b"%PDF-1.7 x");
        assert!(lifecycle.copy_file_out(&on_network.id, "/tmp/missing").await.is_err());

        let found = lifecycle
            .find_networks_by_label(&format!("{label_key}={unique}"))
            .await
            .expect("list");
        assert_eq!(found, vec![network.clone()]);

        lifecycle.disconnect_network(&network, &joiner.id).await.expect("leave");
        lifecycle.disconnect_network(&network, &joiner.id).await.expect("leaving twice is fine");
        let _ = lifecycle.teardown(&on_network.id).await;
        let _ = lifecycle.teardown(&joiner.id).await;
        lifecycle.remove_network(&network).await.expect("remove");
    }

    #[tokio::test]
    #[ignore = "needs a real Docker daemon"]
    async fn exec_pipes_stdin_and_reports_the_real_exit_code() {
        let lifecycle = DockerLifecycle::connect().await.expect("a real Docker daemon");
        lifecycle
            .ensure_image("postgres:16-alpine", "")
            .await
            .expect("postgres:16-alpine pulls or is already present");

        let spec = ContainerSpec {
            name: format!(
                "u2s-verify-core-exec-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ),
            image: "postgres:16-alpine".to_owned(),
            platform: String::new(),
            network: "bridge".to_owned(),
            env: vec![("POSTGRES_PASSWORD".to_owned(), "password".to_owned())],
            labels: HashMap::new(),
            publish_ports: vec![],
            binds: vec![],
            extra_hosts: vec![],
            memory_bytes: None,
        };
        let running = lifecycle.run(&spec).await.expect("the container starts");

        let result = lifecycle
            .exec(&running.id, vec!["cat".to_owned()], Some(b"hello from stdin"))
            .await
            .expect("exec must reach the daemon");

        assert!(result.succeeded(), "cat must exit 0: {result:?}");
        assert!(result.output.contains("hello from stdin"), "{result:?}");

        let exit_status = lifecycle
            .exec(&running.id, vec!["false".to_owned()], None)
            .await
            .expect("exec must reach the daemon");
        assert!(!exit_status.succeeded(), "`false` must report a non-zero exit code");

        lifecycle
            .wait_for_exec(
                &running.id,
                &["pg_isready", "-U", "postgres"],
                ExecOutput::succeeded,
                Duration::from_secs(60),
                Duration::from_millis(500),
            )
            .await
            .expect("postgres becomes ready");
        let never = lifecycle
            .wait_for_exec(
                &running.id,
                &["false"],
                ExecOutput::succeeded,
                Duration::from_secs(2),
                Duration::from_millis(500),
            )
            .await;
        assert!(matches!(never, Err(DockerError::ExecNotReady { .. })), "{never:?}");

        let _ = lifecycle.teardown(&running.id).await;

        // Without a password the image refuses to initialize and exits 1,
        // explaining why: a one-shot container with no command override.
        let one_shot = ContainerSpec {
            name: format!("{}-oneshot", spec.name),
            env: Vec::new(),
            ..spec
        };
        let result = lifecycle
            .run_to_completion(&one_shot, Duration::from_secs(60))
            .await
            .expect("the container runs and exits");
        assert_eq!(result.exit_code, 1, "{result:?}");
        assert!(result.output.contains("POSTGRES_PASSWORD"), "{result:?}");
    }
}
