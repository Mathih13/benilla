# Hosting a LyraCore realm

Set `REALM_BACKEND=lyracore` to use LyraCore accounts. This mode does not require MySQL,
SOAP, or MaNGOS configuration files. It retains the web login, first-login password change,
account invitations and play page. The admin panel cannot change LyraCore gameplay settings,
list Characters, ban, kick or delete game accounts.

Build the browser files with `scripts/web-setup.sh` followed by `scripts/web-build.sh`.
Leave `CARGO_TARGET_DIR` unset for these scripts. Build the host with
`cargo build -p wenilla-realm`. The browser needs WebGPU and a trusted HTTPS origin.

Configure the host through its service environment:

```text
REALM_BACKEND=lyracore
REALM_BIND=127.0.0.1:8090
REALM_PUBLIC_URL=https://your-public-address
REALM_STATE_DIR=/var/lib/wave-defense-web
REALM_WWW=/opt/wave-defense-web/www
REALM_ADDONS=/opt/wave-defense-web/addons
CLIENT_DATA=/srv/wowclient/Data
REALMD_HOST=127.0.0.1
MANGOSD_HOST=127.0.0.1
LYRACORE_GATEWAY_BIN=/opt/wave-defense-web/bin/lyracore-gateway
LYRACORE_SPACETIMEDB_URL=http://127.0.0.1:3001
LYRACORE_DATABASE=wave-defense
```

Supply `LYRACORE_COORDINATOR_TOKEN` from the realm's protected Operator credential file.
Do not put it in source control. Optional `LYRACORE_REALM_CORE` and `LYRACORE_SHARD_MAP`
must match the Gateway's configuration. The host passes only these explicit account settings
to the command; it does not inherit an ambient Gateway environment.

The Gateway must support `provision USER --password-stdin --create-only`, and its Module
must expose `create_account`. LyraCore commit `718daae2` provides both. Creation refuses a
name collision without changing the existing Account. Password rotation uses the existing
provision command without `--create-only`. Across separate Shards, a failed creation can leave
a new Account on only one Shard. The deployed Wave Defense realm uses one Shard.

Use a new state directory. The host pins its account provider in SQLite and refuses a
directory previously used for MaNGOS accounts. Web users receive random game account names;
they do not sign in with an existing Discord or native game account.

On first start, read `REALM_STATE_DIR/setup-token` privately and finish `/setup`. Use
`/admin/users` to invite testers. They must change their temporary password before playing.
Account commands send passwords through stdin and suppress child output. A failed command
does not save new game credentials. If a rotation commits but its reply is lost, retry the
rotation to restore agreement between the realm and web credentials.

Place `WaveDefense/WaveDefense.toc` and the addon's files below `REALM_ADDONS`. Restart the
host after changing that directory because it builds the addon index at startup. The addon
index, files, client assets and WebSocket relay require an authenticated web session. Keep
the original client installation read-only.

`/healthz` reports the selected account provider and client-data availability. In LyraCore
mode, MySQL and SOAP health values are null. A healthy web process alone does not prove that
Gateway provisioning or browser world entry works; verify those against a dedicated test account.
