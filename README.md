# usb-handbrake-rs

Driver em Rust para fazer um **freio de mão analógico USB** (placa genérica **LeafLabs Maple**, USB `1eaf:0024`) funcionar no Linux com a precisão analógica total (0 a 1023) em simuladores de corrida: Assetto Corsa, Richard Burns Rally, jogos via Steam/Proton etc.

## O problema

Ligado no Linux, esse freio de mão não funciona direito por três motivos:

1. O kernel o carrega como um joystick comum, mas os relatórios HID são fora de especificação e geram **32 botões fantasma** que atrapalham os simuladores.
2. A pressão real vem em relatórios brutos de 13 bytes em `/dev/hidrawX`, que os jogos não leem.
3. Os simuladores precisam de um dispositivo válido com eixos analógicos.

## O que este programa faz

Lê os relatórios brutos de `/dev/hidraw*` (por volta de 1000 Hz) e cria, via **uinput**, um joystick virtual chamado `USB Analog Handbrake` com os eixos `ABS_X`, `ABS_Y`, `ABS_RZ` e `ABS_BRAKE` (0 a 1023). Ele emite só eventos de eixo, sem teclas. Se o freio for desconectado e reconectado, o programa volta a detectá-lo sozinho.

Há também uma **calibração guiada** e uma curva de resposta configurável, que o programa relê ao vivo.

## Instalação

Precisa de Rust (`cargo`) e de acesso ao `uinput`.

```bash
# 1. Permissões
sudo usermod -aG input,uinput $USER     # saia e entre na sessão depois

# 2. Compilar e instalar
cargo build --release
install -Dm755 target/release/usb-handbrake-bridge ~/.local/bin/usb-handbrake-bridge

# 3. Ocultar o dispositivo bruto (para só o virtual aparecer)
sudo cp contrib/99-hide-maple.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules && sudo udevadm trigger

# 4. Rodar como serviço de usuário
mkdir -p ~/.config/systemd/user
cp contrib/handbrake-bridge.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now handbrake-bridge
```

> **Steam e Proton:** o programa **não** usa `EVIOCGRAB` no dispositivo bruto de propósito. Travá-lo causa `EBUSY` quando o contêiner `pressure-vessel` da Steam inicializa.

## Calibração

```bash
usb-handbrake-bridge --calibrar
```

Mede o repouso e o curso, mostra uma barra ao vivo e grava os valores em `~/.config/handbrake.toml`. Pode ser feita com o serviço rodando. Para ajustar à mão, copie `handbrake.toml.exemplo` para `~/.config/handbrake.toml`:

| Chave | Significado |
| --- | --- |
| `min` | folga da mola: abaixo disso conta como 0% |
| `max` | ponto de bloqueio: daí em diante conta como 100% |
| `curva` | `1.0` linear; maior que 1 começa mais suave; menor que 1 mais agressivo |
| `inverter` | `true` se o freio estiver montado ao contrário |

O programa confere o arquivo a cada 0,5 s e aplica as mudanças sem reiniciar.

## Compatibilidade

Testado no Linux Mint 22 (Cinnamon), com o freio de mão USB baseado na placa LeafLabs Maple (`1eaf:0024`). Outros freios de mão com esse mesmo ID podem funcionar. Outros IDs exigem mudar `VENDOR_ID`/`PRODUCT_ID` e o formato do relatório em `src/main.rs`.

## Estrutura

- `src/main.rs`: leitura do hidraw e dispositivo virtual uinput.
- `src/calibracao.rs`: curva, limites e recarga ao vivo do arquivo de calibração.
- `src/calibrar.rs`: calibração guiada (`--calibrar`).
- `contrib/`: regra udev e serviço systemd.
