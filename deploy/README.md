# Public Surfd servers on Linux

This setup runs `surf-records` and managed `surf-server` instances under systemd. Nginx serves the records API over HTTPS and reaches records on loopback. Desktop clients connect directly to each room's TLS port.

Use a Linux host with systemd 247 or newer and Nginx with HTTPS support. Set `records.example.com` and `room.example.com` to your DNS names. Provision a PEM certificate chain and key for each hostname. The sample room binds IPv4 on TCP 27165. Advertise an IPv6 address only after you configure an IPv6 listener. Allow public TCP 443 and the chosen room ports in the host firewall. Records uses the private loopback port 27166.

## Install the server binaries and configuration

From the repository root on the host, build the two server binaries using the project Rust toolchain and a native C compiler.

```sh
cargo build --release --locked --no-default-features --features records-service --bin surf-records --bin surf-server
sudo install -m 0755 target/release/surf-records target/release/surf-server /usr/local/bin/
sudo install -m 0644 deploy/surfd-records.service deploy/surfd-room@.service /etc/systemd/system/
sudo install -d -m 0700 /etc/surfd /etc/surfd/records-tls /etc/surfd/rooms /etc/surfd/rooms/boreas-eu
sudo install -m 0600 deploy/records.env.example /etc/surfd/records.env
sudo install -m 0600 deploy/room.env.example /etc/surfd/rooms/boreas-eu.env
```

Edit the two environment files. Generate a random 32-byte key with `openssl rand -hex 32`, and put the same key in the room's `SURF_ROOM_TICKET_KEY` and its `SURF_ROOM_TICKETS` entry on records. Keep each key in the root-owned configuration files. Each additional room uses its own key, ID, listening port, environment file, and directory of certificates. The room ID comes from the systemd instance name. In `SURF_ROOM_TICKETS`, list each room's complete public `tls://host:port` address and separate entries with semicolons.

Put `fullchain.pem` and `privkey.pem` for the records hostname in `/etc/surfd/records-tls/`. Put the pair for the room hostname in `/etc/surfd/rooms/boreas-eu/`. Keep the keys readable only by root. Systemd supplies the room certificate and key to its service at startup. Its state directory stays writable by that room's service account. [systemd credentials and managed directories](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html)

If using Google or X sign-in, configure `SURF_OAUTH_PUBLIC_URL` with the records HTTPS origin and fill in the corresponding provider variables in `records.env`. Register the matching `/v1/oauth/google/callback` or `/v1/oauth/x/callback` with the provider. Set `SURF_ROOM_MODERATOR_IDS` to the native records account IDs that should moderate rooms or review message reports.

## Enable and verify

Install `surfd-records.nginx.conf` in an include loaded by Nginx's `http` block, such as `/etc/nginx/conf.d/surfd-records.conf`. Set its hostname and PEM paths. The proxy caps request bodies at 16 MiB, buffers uploads before they reach records, and replaces `X-Real-IP` with the connection's peer address. Keep `SURF_RECORDS_TRUST_REAL_IP=1` when this proxy accepts client connections directly. The sample permits 32 simultaneous requests from one address and 64 in total. Plan for up to 1 GiB of upload buffering at those limits. [Nginx request buffering](https://nginx.org/en/docs/http/ngx_http_proxy_module.html#proxy_request_buffering) · [Nginx connection limits](https://nginx.org/en/docs/http/ngx_http_limit_conn_module.html)

```sh
sudo systemd-analyze verify /etc/systemd/system/surfd-records.service /etc/systemd/system/surfd-room@.service
sudo nginx -t
sudo systemctl daemon-reload
sudo systemctl enable --now surfd-records.service surfd-room@boreas-eu.service
sudo systemctl enable --now nginx.service
sudo systemctl reload nginx.service
curl --fail --show-error https://records.example.com/v1/ready
```

From a player network, point the desktop executable at the same records origin and run `bevy-surf --check-room boreas-eu`. On Windows PowerShell, set `$env:SURF_RECORDS_URL = 'https://records.example.com'` before running `bevy-surf.exe --check-room boreas-eu`. This tests the public records connection, room directory, certificate and guest ticket by joining and leaving the room.

Use `bevy-surf --room-status boreas-eu` for routine monitoring. It checks the configured room ID and TLS connection, then prints JSON with `reachable`, `full`, `discovery_ms`, and the room's current map and occupancy. A reachable full room exits successfully with `full: true`. Directory, certificate, identity, and connection failures exit with an error. This command uses a discovery probe and consumes no player slot or join ticket.

## Operate the services

- Records keeps its SQLite database under `/var/lib/surfd-records` and its downloaded map cache under `/var/cache/surfd-maps`. Each room keeps its moderation file under `/var/lib/surfd-rooms/<room-id>`. Preserve the state directories across binary upgrades. [systemd directory persistence](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html)
- Use `journalctl -u surfd-records.service -u surfd-room@boreas-eu.service` for service output. Nginx's sample access log includes the request path and upstream time. It omits the query string, including OAuth callback codes.
- Renew the PEM files and reload Nginx for the records certificate. Restart the affected room unit to load its renewed certificate.
- Use the desktop moderation menu with a signed moderator account for a managed room. On `systemctl stop` or `systemctl restart`, the room closes admission, tells connected players it is shutting down, and drains connections for up to two seconds. The local server console accepts `quit` or Ctrl+C for the same shutdown.
- Save a live SQLite snapshot with `sudo /usr/local/bin/surf-records --data /var/lib/surfd-records --backup /path/to/new-records-backup.sqlite3`. Preserve the room moderation JSON and root-owned environment files separately. Verify a database restore in a separate state directory and on a separate loopback port before switching the public service.
