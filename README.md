# sunmimgr

Multiplataforma (Rust + GPUI) reimplementação das ferramentas Sunmi
`SunmiDownloader` + `sunmitool` + `sunmiboot` para gerenciamento de terminais
POS Sunmi (instalação de apps, recursos, logs e flash de ROM).

## Estrutura

```
sunmimgr/
├── Cargo.toml              # workspace
└── crates/
    ├── sunmi-core/         # domínio: tipos de arquivo, config, script, package
    ├── sunmi-adb/          # backends adb/fastboot (v1: spawn dos binários do sistema)
    └── sunmi-ui/           # aplicação (hoje: smoke-test CLI; depois: GPUI)
```

## Arquitetura

### sunmi-core
- `filetype::FileKind` — port do enum `filetype` + heurísticas `GuessFileType`
  do original (.apk → App, .so → So, *_ota.zip/inc-*.zip → Ota,
  *package*.zip → Package, outro .zip → AppCrp, `sunmiboot_scatter.txt` → Rom).
- `config::DownloadConfig` — compatível com o `download_config.json` original.
- `script::ScriptFile` — compatível com `*_script.json` do original.
- `package::MultiPackageConfig` — compatível com `multi_package_config.json`.

### sunmi-adb
O `sunmitool.exe` da Sunmi é um fork do adb do AOSP cujo único extra é
encaminhar o serviço `shell:systool ...` para o daemon `systool` do firmware
do terminal. Como o daemon roda no dispositivo, um cliente adb padrão basta —
não há necessidade do fork da Sunmi.

**Firmware de produção (bloqueio de shell)**: builds seguros (SecLevel=2)
recusam comandos `shell:` genéricos (`echo`, `ls`, `rm` → `error: closed`),
mas o roteamento do daemon permanece na whitelist do adbd (`shell:systool ...`
responde normalmente). Comandos suportados pelo daemon:

| Comando systool | Uso |
|---|---|
| `list app\|preinstall-apk\|lib` | listar conteúdo instalado |
| `install app\|preinstall-apk\|lib\|cert\|crp\|arp <file>` | instalar (arquivo **já no terminal**) |
| `install app_param:<app> <file>` | instalar parâmetro em app |
| `remove app\|preinstall-apk\|lib <name>` | remover |
| `get posinfo` | telemetria do terminal (SN, versões FW/CRP/ARP/SE, ...) |
| `set time <yyyy/MM/dd-HH:mm:ss>` | sincronizar relógio |
| `write apn <file>` / `update otapackage <file>` | APN / OTA |
| `packlogs [modemlog]` + `pull /data/public/packlogs.zip` | coleta de logs |
| `start-modemlog` | abrir debug de modem (não-Qualcomm) |

**Fluxo de instalação (validado em P2-A11 4.0.69 produção):**
1. `systool version` → lê `systool_tmp_dir` (vazio na produção → fallback
   `/data/local/tmp`)
2. `adb push <arquivo> <tmp_dir>/<nome>` (serviço sync, não bloqueado)
3. `systool install <tipo> <caminho-no-terminal>` → `{"ret":0,"msg":"OK"}`
4. Arquivo staging permanece no tmp (o daemon não expõe `rm`; `shell: rm` é
   bloqueado em produção)
5. Nunca invocar `systool` sem argumentos — segfault no daemon e shell do
   adbd fica indisponível até replug

Flash de ROM usa fastboot padrão guiado por `sunmiboot_scatter.txt`
(`flash <partição> <arquivo>` / `erase` / `reboot`).

**v1**: spawn dos binários `adb`/`fastboot` do sistema.
**v2 (planejado)**: protocolo ADB/fastboot nativo em Rust (`nusb`), progresso
real de push e sem dependência externa.

### sunmi-ui
Planejada com [GPUI](https://github.com/zed-industries/zed) (framework do Zed).
Antes de ativar a dependência, validar o suporte do backend Linux/Windows do
GPUI na versão atual — a dependência fica comentada no `Cargo.toml` até lá.

## Roadmap

- [x] Scaffold do workspace
- [x] `sunmi-core`: modelos de domínio
- [x] `sunmi-adb`: wrapper adb/fastboot (spawn)
- [x] Loop de download (App/Apn/AppCrp — 1 task por dispositivo, stop/timeout)
- [x] Fluxo de instalação validado em terminal real de produção
      (P2-A11 4.0.69: App `br.com.fly.pay.pdv.p2` 60→61, AppCrp
      `{"ret":0,"msg":"OK"}`, Apn `{"ret":0}` — push sync → systool install)
- [ ] Fluxo ROM (reboot bootloader → scatter → fastboot)
- [ ] Package/Script (zip com senha + executor)
- [x] Device info (`get posinfo`) + sincronização de hora
- [ ] UI GPUI
- [ ] ADB nativo em Rust (v2)
