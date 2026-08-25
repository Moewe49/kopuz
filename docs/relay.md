# Running your own relay

The desktop analyses how your library sounds and groups it into mixes. Your
phone cannot do that — there is no verified ONNX runtime build for
aarch64-android, and even if there were, an evening of CPU and a gigabyte of
mobile data to redo work the desktop already did is not a trade worth making.

So the result has to travel. Two devices behind different routers have no path
to each other that does not pass through something both can reach. That
something is this: a small program you run yourself, on a machine you own.

No account. No third party holding your listening history. Nothing to pay for
and nothing that can be shut down from the outside. It is the same bargain the
share codes made, one step further along.

## What it actually does

An authenticated key-value store, and nothing more:

```
PUT  /v1/state/{key}   Authorization: Bearer <token>   body = bytes
GET  /v1/state/{key}   Authorization: Bearer <token>   → bytes + version
```

Every value carries a version, so a reader can say what it already has and be
told "nothing new" in eleven bytes rather than being sent fifty kilobytes it
already holds. On mobile data that is the difference between a feature and a
nuisance.

It is **not** a sync engine. No merge, no conflict resolution, no history. The
desktop authors a mix set; the phone reads it. Building three-way merges for
data that is regenerated from scratch every day would be work spent on nothing.

## The secret

There are no accounts, because there is one person. One shared string, held by
the relay and by each of your devices, is the whole of the security. Generate a
long random one:

```bash
openssl rand -base64 32
```

On Windows without OpenSSL:

```bash
powershell -c "[Convert]::ToBase64String((1..32|%{Get-Random -Max 256}))"
```

Anything under 16 characters is refused at startup. A short token is worse than
a missing one, because it looks like it is working.

## On your PC, now

```bash
KOPUZ_RELAY_TOKEN=<your token> cargo run --release -p relay --features server --bin kopuz-relay
```

| variable | default | |
|---|---|---|
| `KOPUZ_RELAY_TOKEN` | — | required |
| `KOPUZ_RELAY_BIND` | `0.0.0.0:8484` | |
| `KOPUZ_RELAY_DATA` | `./kopuz-relay-state.json` | survives restarts |

In the app, on **every** device: Settings → *My own relay*. Address and token,
then **Test**. It answers straight away — a typo found now beats "the mixes
never showed up on my phone" found next week.

The address the phone needs is your PC's LAN address, not `localhost`:

```bash
ipconfig | findstr IPv4
```

So `192.168.1.50:8484`, and both devices on the same wifi. The scheme can be
left off.

### Windows will block it first

The first time the relay binds a port, Windows shows a firewall prompt. Nobody
reads it, and dismissing it does not mean "ask again later" -- it writes a
**Block** rule, and the relay then answers the machine it runs on perfectly
while being invisible to every other device. Which looks exactly like the app
being broken.

Check for the rules that dismissal left behind:

```powershell
Get-NetFirewallApplicationFilter | Where-Object { $_.Program -like '*kopuz-relay*' } | Get-NetFirewallRule | Select-Object DisplayName, Action, Profile
```

If any say `Block`, remove them and allow the port instead. As administrator:

```powershell
Get-NetFirewallApplicationFilter | Where-Object { $_.Program -like '*kopuz-relay*' } | Get-NetFirewallRule | Remove-NetFirewallRule
New-NetFirewallRule -DisplayName "Kopuz relay" -Direction Inbound -Protocol TCP -LocalPort 8484 -RemoteAddress LocalSubnet -Action Allow
```

`-RemoteAddress LocalSubnet` is the part worth keeping: it opens the port to
the flat, not to whatever coffee-shop wifi this laptop joins next.

Check which profile the network is on, too:

```powershell
Get-NetConnectionProfile | Select-Object InterfaceAlias, NetworkCategory
```

A home network reported as `Public` is Windows being cautious about a network
you trust. Setting it to `Private` is a reasonable thing to do for your own
flat and a bad thing to do anywhere else, so it is left to you.

## On the MS-01, later

### Docker

```bash
docker build -t kopuz-relay -f crates/relay/Dockerfile .
docker run -d --name kopuz-relay --restart unless-stopped \
  -p 8484:8484 \
  -e KOPUZ_RELAY_TOKEN=<your token> \
  -e KOPUZ_RELAY_DATA=/data/state.json \
  -v kopuz-relay-data:/data \
  kopuz-relay
```

### systemd

Copy the binary to `/usr/local/bin/kopuz-relay`, put the token in
`/etc/kopuz-relay.env` as `KOPUZ_RELAY_TOKEN=…` with mode `600`, then:

```ini
[Unit]
Description=Kopuz relay
After=network-online.target

[Service]
EnvironmentFile=/etc/kopuz-relay.env
Environment=KOPUZ_RELAY_DATA=/var/lib/kopuz-relay/state.json
ExecStart=/usr/local/bin/kopuz-relay
DynamicUser=yes
StateDirectory=kopuz-relay
Restart=on-failure
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes

[Install]
WantedBy=multi-user.target
```

`DynamicUser` and `ProtectSystem=strict` cost nothing here and mean a bug in
this program cannot reach anything but its own state directory.

## Reaching it from outside the flat

**The token authenticates. It does not encrypt.** Over plain HTTP to a public
address, anyone on the path reads the secret and then reads and writes
everything the relay holds. The app warns you on the settings screen when the
address you have typed would do that. A longer token does not help.

Two ways that do:

**Tailscale, recommended.** Install it on the MS-01 and on both devices. The
relay stays bound to the tailnet address, nothing is exposed to the internet at
all, and the addresses work from anywhere:

```bash
tailscale ip -4        # → 100.x.y.z
```

Use `100.x.y.z:8484` in the app. Tailscale hands out addresses from 100.64/10,
which the app recognises as your own network, so it will not warn.

**A reverse proxy with a certificate.** Caddy needs two lines:

```
kopuz.example.net {
	reverse_proxy localhost:8484
}
```

Then use `https://kopuz.example.net` in the app.

TLS is deliberately not built in. It would mean owning certificate renewal and
a decade of protocol decisions for one small feature, and doing it worse than
the proxy already running on that machine.

## Checking it is alive

```bash
curl -s http://<host>:8484/healthz
```

`/healthz` needs no token on purpose: it says nothing about your data, and it
lets a container runtime or a proxy watch the process without holding the
secret.

To see what it is holding:

```bash
curl -s -H "Authorization: Bearer <token>" "http://<host>:8484/v1/state/mixes?have=0" \
  | python3 -c "import json,sys,base64; d=json.load(sys.stdin)['Value']; \
      m=json.loads(base64.b64decode(d['bytes'])); \
      print(d['version'], [x['name'] for x in m['mixes']])"
```

## When it does not work

| what you see | what it is |
|---|---|
| *the relay rejected the token* | the two sides disagree — check for a trailing space |
| *could not reach the relay* | wrong address, firewall, or not on the same network |
| the phone shows different mixes | the desktop has not published yet; it does so after it next rebuilds them, or turn *Analyse my music* on |
| the phone shows no mixes at all | nothing published and no listening history to fall back on |

The relay prints a line for every write. It never prints a token, and neither
does the app.
