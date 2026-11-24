mod calibracao;
mod calibrar;

use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use calibracao::CalibracaoAoVivo;

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode,
    UinputAbsSetup,
};

const VENDOR_ID: &str = "1EAF";
const PRODUCT_ID: &str = "0024";

fn find_handbrake_hidraw() -> Option<PathBuf> {
    let hidraw_dir = Path::new("/sys/class/hidraw");
    let entries = fs::read_dir(hidraw_dir).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();
        let uevent_path = path.join("device/uevent");

        if let Ok(content) = fs::read_to_string(&uevent_path) {
            let content_upper = content.to_ascii_uppercase();
            if content_upper.contains(VENDOR_ID) && content_upper.contains(PRODUCT_ID) {
                if let Some(file_name) = path.file_name() {
                    let dev_path = PathBuf::from("/dev").join(file_name);
                    if dev_path.exists() {
                        return Some(dev_path);
                    }
                }
            }
        }
    }

    None
}

fn create_virtual_device() -> io::Result<VirtualDevice> {
    // Declaramos as capacidades de BTN_JOYSTICK e BTN_TRIGGER no descritor
    // para garantir total compatibilidade com udev, joydev (/dev/input/jsX),
    // Steam Input, Proton/Wine e DirectInput.
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::BTN_TRIGGER); // 0x120 (alias BTN_JOYSTICK)
    keys.insert(KeyCode::BTN_THUMB);   // 0x121

    // Eixos analógicos completos com ABS_X preservado para manter
    // a indexação esperada por Assetto Corsa, RBR e DirectInput:
    // Axis 0: ABS_X (default 0)
    // Axis 1: ABS_Y (valor = val)
    // Axis 2: ABS_RZ (valor = val, padrão comum de freio de mão)
    // Axis 3: ABS_BRAKE (valor = val)
    let abs_x = UinputAbsSetup::new(
        AbsoluteAxisCode::ABS_X,
        AbsInfo::new(0, 0, 1023, 0, 0, 0),
    );
    let abs_y = UinputAbsSetup::new(
        AbsoluteAxisCode::ABS_Y,
        AbsInfo::new(0, 0, 1023, 0, 0, 0),
    );
    let abs_rz = UinputAbsSetup::new(
        AbsoluteAxisCode::ABS_RZ,
        AbsInfo::new(0, 0, 1023, 0, 0, 0),
    );
    let abs_brake = UinputAbsSetup::new(
        AbsoluteAxisCode::ABS_BRAKE,
        AbsInfo::new(0, 0, 1023, 0, 0, 0),
    );

    VirtualDevice::builder()?
        .name("USB Analog Handbrake")
        .input_id(InputId::new(BusType::BUS_USB, 0x1EAF, 0x0025, 1))
        .with_keys(&keys)?
        .with_absolute_axis(&abs_x)?
        .with_absolute_axis(&abs_y)?
        .with_absolute_axis(&abs_rz)?
        .with_absolute_axis(&abs_brake)?
        .build()
}

/// Valor bruto (0..=1023) de um relatório HID do freio: ID 0x14, eixo nos bytes 11-12.
fn raw_do_relatorio(buf: &[u8]) -> Option<i32> {
    if buf.len() < 13 || buf[0] != 0x14 {
        return None;
    }
    let (b11, b12) = (buf[11] as u16, buf[12] as u16);
    Some((((b11 | (b12 << 8)) >> 6) & 0x03FF) as i32)
}

fn main() {
    if std::env::args().any(|a| a == "--calibrar") {
        std::process::exit(calibrar::executar());
    }
    println!("Iniciando ponte do freio de mão analógico (Rust)...");

    let mut virtual_dev = match create_virtual_device() {
        Ok(dev) => {
            println!("Dispositivo virtual uinput joystick criado com sucesso!");
            dev
        }
        Err(e) => {
            eprintln!("Erro fatal ao criar dispositivo uinput: {e}");
            std::process::exit(1);
        }
    };

    let mut last_val: i32 = -1;
    let mut buf = [0u8; 64];
    // Calibração em ~/.config/handbrake.toml, relida ao vivo (a cada 0,5 s confere se o arquivo mudou)
    let caminho = calibracao::caminho_padrao();
    println!("Calibração: {}", caminho.display());
    let mut calib = CalibracaoAoVivo::new(&caminho);
    let mut ultima_checagem = Instant::now();

    loop {
        let hidraw_path = match find_handbrake_hidraw() {
            Some(path) => path,
            None => {
                println!("Aguardando conexão do freio de mão USB (1EAF:0024)...");
                sleep(Duration::from_secs(2));
                continue;
            }
        };

        println!("Freio de mão detectado em {}. Conectando...", hidraw_path.display());

        let mut file = match OpenOptions::new().read(true).open(&hidraw_path) {
            Ok(f) => {
                println!("Conectado com sucesso a {}! Monitorando pressão analógica...", hidraw_path.display());
                f
            }
            Err(e) => {
                eprintln!("Erro ao abrir {}: {}. Tentando novamente em 2s...", hidraw_path.display(), e);
                sleep(Duration::from_secs(2));
                continue;
            }
        };

        loop {
            match file.read(&mut buf) {
                Ok(0) => {
                    eprintln!("EOF recebido do dispositivo hidraw. Desconectado.");
                    break;
                }
                Ok(n) => {
                    if let Some(raw) = raw_do_relatorio(&buf[..n]) {
                        if ultima_checagem.elapsed() >= Duration::from_millis(500) {
                            ultima_checagem = Instant::now();
                            calib.recarregar_se_mudou();
                        }
                        let val = calib.atual.aplicar(raw);

                        // Emissão analógica pura:
                        // Dispara exclusivamente eixos ABS, ZERO eventos de tecla/botão em runtime!
                        if val != last_val {
                            last_val = val;

                            let events = [
                                InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Y.0, val),
                                InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_RZ.0, val),
                                InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_BRAKE.0, val),
                            ];

                            if let Err(e) = virtual_dev.emit(&events) {
                                eprintln!("Erro ao emitir eventos uinput: {e}");
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Freio desconectado ou erro de leitura: {e}");
                    break;
                }
            }
        }

        // Aguarda 1s antes de tentar redetectar
        sleep(Duration::from_secs(1));
    }
}
