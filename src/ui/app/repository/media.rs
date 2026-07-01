use std::net::{ToSocketAddrs, UdpSocket};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use log::{debug, warn};
use md5::Md5;
use reqwest::blocking::Client;
use sha1::{Digest, Sha1};

use crate::ui::app::{DecodedImagePayload, Foxy, ImageLoadResult};
use crate::ui::types::{RepositoryServer, ServerOnlineStatus};

/// Shared HTTP client for image downloads, enabling TLS session reuse and
/// connection pooling across all image fetch operations.
static IMAGE_HTTP_CLIENT: std::sync::LazyLock<Client> = std::sync::LazyLock::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("failed to build shared image HTTP client: check TLS/system configuration")
});

impl Foxy {
    pub fn query_steam_a2s_info(address: &str, port: &str) -> Result<u32, std::io::Error> {
        let port_number: u16 = port
            .parse()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let new_port_number = port_number + 1;
        let socket_addr = format!("{}:{}", address, new_port_number);

        let resolved = socket_addr.to_socket_addrs()?.next().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                "Unable to resolve socket address",
            )
        })?;

        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;

        let mut request = vec![
            0xFF, 0xFF, 0xFF, 0xFF, 0x54, b'S', b'o', b'u', b'r', b'c', b'e', b' ', b'E', b'n',
            b'g', b'i', b'n', b'e', b' ', b'Q', b'u', b'e', b'r', b'y', 0x00,
        ];

        fn send_and_receive(
            socket: &UdpSocket,
            buf: &mut [u8],
            request: &[u8],
            server: std::net::SocketAddr,
        ) -> Result<usize, std::io::Error> {
            socket.send_to(request, server)?;
            let (len, _) = socket.recv_from(buf)?;
            Ok(len)
        }

        let mut recv_buf = [0u8; 2048];
        let mut len = send_and_receive(&socket, &mut recv_buf, &request, resolved)?;

        if len >= 5 && recv_buf[4] == 0x41 {
            let challenge = &recv_buf[5..9];
            request.extend_from_slice(challenge);
            len = send_and_receive(&socket, &mut recv_buf, &request, resolved)?;
        }

        if len < 5 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "A2S_INFO response too short",
            ));
        }
        if recv_buf[4] != 0x49 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Unexpected header 0x{:X} in A2S_INFO response", recv_buf[4]),
            ));
        }

        let mut index = 5;
        index += 1;

        fn read_cstring(buf: &[u8], start: &mut usize) -> Option<String> {
            let mut collected = Vec::new();
            while *start < buf.len() {
                let b = buf[*start];
                *start += 1;
                if b == 0 {
                    break;
                }
                collected.push(b);
            }
            String::from_utf8(collected).ok()
        }

        let _server_name = read_cstring(&recv_buf, &mut index).unwrap_or_default();
        let _map_name = read_cstring(&recv_buf, &mut index).unwrap_or_default();
        let _folder_name = read_cstring(&recv_buf, &mut index).unwrap_or_default();
        let _game_name = read_cstring(&recv_buf, &mut index).unwrap_or_default();

        if index + 2 > len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Truncated response reading AppID",
            ));
        }
        index += 2;

        if index >= len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Truncated response reading player count",
            ));
        }
        let player_count = recv_buf[index];

        Ok(player_count as u32)
    }

    pub fn get_server_status(&mut self, server: &RepositoryServer) -> ServerOnlineStatus {
        let key = (server.address.clone(), server.port.clone());
        let now = Instant::now();
        let server_status_ttl = Duration::from_secs(30);

        if let Some(entry) = self.server_statuses.get_mut(&key)
            && now.duration_since(entry.last_check) < server_status_ttl
        {
            return entry.status;
        }

        self.spawn_query_thread(server);
        self.server_statuses
            .get(&key)
            .map(|c| c.status)
            .unwrap_or(ServerOnlineStatus::Offline)
    }

    pub fn force_refresh_server_status(&mut self, server: &RepositoryServer) {
        self.server_refresh_indicator_until.insert(
            (server.address.clone(), server.port.clone()),
            Instant::now() + Duration::from_secs(1),
        );
        self.needs_repaint = true;
        self.spawn_query_thread(server);
    }

    pub fn is_server_refresh_indicator_active(&mut self, server: &RepositoryServer) -> bool {
        let key = (server.address.clone(), server.port.clone());
        let now = Instant::now();

        match self.server_refresh_indicator_until.get(&key).copied() {
            Some(until) if until > now => true,
            Some(_) => {
                self.server_refresh_indicator_until.remove(&key);
                false
            }
            None => false,
        }
    }

    pub fn spawn_query_thread(&mut self, server: &RepositoryServer) {
        let address = server.address.clone();
        let port = server.port.clone();
        let key = (address.clone(), port.clone());
        if !self.pending_server_queries.insert(key) {
            return;
        }
        let tx = self.updates_sender.clone();
        let repaint_ctx = self.repaint_ctx.clone();

        let handle = std::thread::spawn(move || {
            let new_status = match Self::query_steam_a2s_info(&address, &port) {
                Ok(players) => ServerOnlineStatus::Online { players },
                Err(_) => ServerOnlineStatus::Offline,
            };
            if tx.send((address, port, new_status)).is_ok() {
                Self::request_background_repaint(repaint_ctx.as_ref());
            }
        });

        self.pending_queries.push(handle);
    }

    fn image_checksum_matches(bytes: &[u8], expected_hex: &str) -> (bool, String) {
        let expected = expected_hex.trim().to_ascii_uppercase();
        if expected.is_empty() {
            return (true, String::new());
        }

        if expected.len() == 32 {
            let mut hasher = Md5::new();
            hasher.update(bytes);
            let actual = hex::encode_upper(hasher.finalize());
            return (actual == expected, actual);
        }

        if expected.len() == 40 {
            let mut hasher = Sha1::new();
            hasher.update(bytes);
            let actual = hex::encode_upper(hasher.finalize());
            return (actual == expected, actual);
        }

        if crate::core::utils::content_hash::is_blake3_checksum(&expected) {
            let actual_blake3 = blake3::hash(bytes).to_hex().to_uppercase();
            return (actual_blake3 == expected, actual_blake3);
        }

        let mut md5_hasher = Md5::new();
        md5_hasher.update(bytes);
        let actual_md5 = hex::encode_upper(md5_hasher.finalize());

        let mut sha1_hasher = Sha1::new();
        sha1_hasher.update(bytes);
        let actual_sha1 = hex::encode_upper(sha1_hasher.finalize());

        let matches = expected == actual_md5 || expected == actual_sha1;
        (matches, format!("md5={}, sha1={}", actual_md5, actual_sha1))
    }

    fn image_cache_path(checksum_hex: &str) -> PathBuf {
        let mut images_dir = Self::get_config_directory();
        images_dir.push("images");
        if !images_dir.exists() && std::fs::create_dir_all(&images_dir).is_err() {
            warn!("Failed to create image cache directory");
        }
        images_dir.join(format!("{}.png", checksum_hex))
    }

    fn fetch_image_bytes_with_retry(full_url: &str) -> Result<Vec<u8>, String> {
        const IMAGE_FETCH_RETRIES: usize = 2;

        let client = &*IMAGE_HTTP_CLIENT;

        let mut last_error = String::new();
        for attempt in 1..=IMAGE_FETCH_RETRIES {
            match client.get(full_url).send() {
                Ok(response) => {
                    if !response.status().is_success() {
                        last_error = format!("HTTP {}", response.status());
                    } else {
                        match response.bytes() {
                            Ok(bytes) => return Ok(bytes.to_vec()),
                            Err(err) => {
                                last_error = format!("Failed to read response body: {}", err);
                            }
                        }
                    }
                }
                Err(err) => {
                    last_error = err.to_string();
                }
            }

            if attempt < IMAGE_FETCH_RETRIES {
                warn!(
                    "Retrying image download {}/{} for {}",
                    attempt + 1,
                    IMAGE_FETCH_RETRIES,
                    full_url
                );
            }
        }

        Err(format!(
            "Failed to download image after {} attempts: {}",
            IMAGE_FETCH_RETRIES, last_error
        ))
    }

    fn load_or_fetch_image_bytes(
        base_url: &str,
        relative_path: &str,
        checksum_hex: &str,
    ) -> Result<Vec<u8>, String> {
        let local_png = Self::image_cache_path(checksum_hex);
        if local_png.exists() {
            match std::fs::read(&local_png) {
                Ok(bytes) => {
                    let (checksum_ok, actual_checksum_hex) =
                        Self::image_checksum_matches(&bytes, checksum_hex);
                    if checksum_ok {
                        debug!("Using cached image {}", checksum_hex);
                        return Ok(bytes);
                    }
                    warn!(
                        "Cached image checksum mismatch for {} (expected {}, got {})",
                        checksum_hex, checksum_hex, actual_checksum_hex
                    );
                    let _ = std::fs::remove_file(&local_png);
                }
                Err(err) => {
                    warn!("Failed to read cached image {}: {}", checksum_hex, err);
                }
            }
        }

        let full_url = format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            relative_path.trim_start_matches('/')
        );
        let bytes = Self::fetch_image_bytes_with_retry(&full_url)?;
        let (checksum_ok, actual_checksum_hex) = Self::image_checksum_matches(&bytes, checksum_hex);
        if !checksum_ok {
            return Err(format!(
                "Checksum mismatch for {} (expected {}, got {})",
                full_url, checksum_hex, actual_checksum_hex
            ));
        }

        if let Err(err) = std::fs::write(&local_png, &bytes) {
            warn!("Failed to cache downloaded image {}: {}", checksum_hex, err);
        }

        Ok(bytes)
    }

    fn fetch_decode_image_job(
        base_url: &str,
        relative_path: &str,
        checksum_hex: &str,
    ) -> Result<DecodedImagePayload, String> {
        let img_bytes = Self::load_or_fetch_image_bytes(base_url, relative_path, checksum_hex)?;
        let image = image::load_from_memory(&img_bytes)
            .map_err(|err| format!("Failed to decode image {}: {}", checksum_hex, err))?
            .to_rgba8();
        let (width, height) = image.dimensions();
        Ok(DecodedImagePayload {
            size: [width as usize, height as usize],
            rgba: image.into_raw(),
        })
    }

    pub(in crate::ui::app) fn poll_image_results(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.image_result_rx.try_recv() {
            let ImageLoadResult {
                checksum_hex,
                is_icon,
                payload,
            } = result;
            self.pending_image_jobs
                .remove(&(checksum_hex.clone(), is_icon));

            let existing_texture = if is_icon {
                self.cached_icons
                    .get(&checksum_hex)
                    .cloned()
                    .or_else(|| self.cached_repo_images.get(&checksum_hex).cloned())
            } else {
                self.cached_repo_images
                    .get(&checksum_hex)
                    .cloned()
                    .or_else(|| self.cached_icons.get(&checksum_hex).cloned())
            };
            if let Some(texture) = existing_texture {
                if is_icon {
                    self.cached_icons
                        .entry(checksum_hex.clone())
                        .or_insert(texture.clone());
                } else {
                    self.cached_repo_images
                        .entry(checksum_hex.clone())
                        .or_insert(texture.clone());
                }
                self.reuse_tracked_texture_bytes(&checksum_hex, is_icon);
                self.needs_repaint = true;
                continue;
            }

            match payload {
                Ok(decoded) => {
                    let bytes = decoded
                        .size
                        .iter()
                        .copied()
                        .product::<usize>()
                        .saturating_mul(4);
                    let texture = ctx.load_texture(
                        &checksum_hex,
                        egui::ColorImage::from_rgba_unmultiplied(decoded.size, &decoded.rgba),
                        Default::default(),
                    );
                    self.remember_loaded_texture_bytes(&checksum_hex, is_icon, bytes);
                    if is_icon {
                        self.cached_icons.insert(checksum_hex, texture);
                    } else {
                        self.cached_repo_images.insert(checksum_hex, texture);
                    }
                    self.needs_repaint = true;
                }
                Err(err) => {
                    warn!("Image load failed: {}", err);
                }
            }
        }
    }

    pub fn download_and_load_image(
        &mut self,
        _ctx: &egui::Context,
        base_url: &str,
        relative_path: &str,
        checksum_hex: &str,
        is_icon: bool,
    ) -> Option<egui::TextureHandle> {
        if checksum_hex.is_empty() {
            return None;
        }

        if is_icon {
            if let Some(texture) = self.cached_icons.get(checksum_hex).cloned() {
                return Some(texture);
            }
            if let Some(texture) = self.cached_repo_images.get(checksum_hex).cloned() {
                self.cached_icons
                    .insert(checksum_hex.to_string(), texture.clone());
                self.reuse_tracked_texture_bytes(checksum_hex, true);
                return Some(texture);
            }
        } else {
            if let Some(texture) = self.cached_repo_images.get(checksum_hex).cloned() {
                return Some(texture);
            }
            if let Some(texture) = self.cached_icons.get(checksum_hex).cloned() {
                self.cached_repo_images
                    .insert(checksum_hex.to_string(), texture.clone());
                self.reuse_tracked_texture_bytes(checksum_hex, false);
                return Some(texture);
            }
        }

        if !self
            .pending_image_jobs
            .insert((checksum_hex.to_string(), is_icon))
        {
            return None;
        }

        let tx = self.image_result_tx.clone();
        let checksum = checksum_hex.to_string();
        let base = base_url.to_string();
        let relative = relative_path.to_string();
        let repaint_ctx = self.repaint_ctx.clone();
        std::thread::spawn(move || {
            let payload = Self::fetch_decode_image_job(&base, &relative, &checksum);
            if tx
                .send(ImageLoadResult {
                    checksum_hex: checksum,
                    is_icon,
                    payload,
                })
                .is_ok()
            {
                Self::request_background_repaint(repaint_ctx.as_ref());
            }
        });

        None
    }
}
