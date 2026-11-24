//! `usb-handbrake-bridge --calibrar`: calibração guiada no terminal. Lê o freio direto do hidraw (dá para rodar com o
//! serviço ligado: cada leitor recebe uma cópia dos relatórios), mede o repouso e o curso, mostra a barra ao vivo e
//! grava `min`/`max`/`inverter` no `handbrake.toml`. O serviço recarrega sozinho.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use crate::calibracao::{self, Calibracao, RAW_MAX};
use crate::{find_handbrake_hidraw, raw_do_relatorio};

/// Linha de barra: `[#####.....]  512 / 1023`.
fn barra(valor: i32, largura: usize) -> String {
    let cheio = (valor.clamp(0, RAW_MAX) as usize * largura) / RAW_MAX as usize;
    format!("[{}{}] {:4} / {}", "#".repeat(cheio), ".".repeat(largura - cheio), valor, RAW_MAX)
}

/// Thread que lê o freio e manda cada valor bruto.
fn ler_freio() -> io::Result<Receiver<i32>> {
    let caminho = find_handbrake_hidraw().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "freio (1EAF:0024) não encontrado"))?;
    let mut f = OpenOptions::new().read(true).open(&caminho)?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0u8; 64];
        while let Ok(n) = f.read(&mut buf) {
            if n == 0 {
                break;
            }
            if let Some(raw) = raw_do_relatorio(&buf[..n]) {
                if tx.send(raw).is_err() {
                    break;
                }
            }
        }
    });
    Ok(rx)
}

/// Thread que avisa quando o usuário aperta Enter.
fn enters() -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut linha = String::new();
        while io::stdin().read_line(&mut linha).map(|n| n > 0).unwrap_or(false) {
            if tx.send(std::mem::take(&mut linha)).is_err() {
                break;
            }
        }
    });
    rx
}

/// Mostra a barra ao vivo até o Enter; devolve (menor, maior) lidos no período. `None` se o freio sumiu.
fn medir_ate_enter(freio: &Receiver<i32>, enter: &Receiver<String>, prompt: &str) -> Option<(i32, i32)> {
    println!("{prompt}");
    let (mut menor, mut maior, mut atual) = (i32::MAX, i32::MIN, None);
    let inicio = Instant::now();
    loop {
        match freio.recv_timeout(Duration::from_millis(30)) {
            Ok(v) => {
                atual = Some(v);
                menor = menor.min(v);
                maior = maior.max(v);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(v) = atual {
            print!("\r  {}   (faixa {menor}..{maior})   ", barra(v, 40));
            let _ = io::stdout().flush();
        }
        // espera ao menos meio segundo de leitura antes de aceitar o Enter
        if inicio.elapsed() > Duration::from_millis(500) && enter.try_recv().is_ok() {
            println!();
            return atual.map(|_| (menor, maior));
        }
    }
}

fn perguntar(enter: &Receiver<String>, prompt: &str) -> bool {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let r = enter.recv().unwrap_or_default();
    !r.trim().to_lowercase().starts_with('n')
}

pub fn executar() -> i32 {
    println!("=== Calibração do freio de mão ===\n");
    let freio = match ler_freio() {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!("Não deu para ler o freio: {e}.\nConfira se ele está conectado (e a permissão do /dev/hidraw*).");
            return 1;
        }
    };
    let enter = enters();
    let caminho = calibracao::caminho_padrao();
    let atual = calibracao::CalibracaoAoVivo::new(&caminho).atual;

    let Some(repouso) = medir_ate_enter(&freio, &enter, "1) SOLTE o freio e deixe parado. Aperte Enter.") else {
        eprintln!("O freio desconectou.");
        return 1;
    };
    let Some((menor, maior)) = medir_ate_enter(
        &freio,
        &enter,
        "\n2) PUXE o freio até o fim (onde você quer 100%), algumas vezes. Depois solte e aperte Enter.",
    ) else {
        eprintln!("O freio desconectou.");
        return 1;
    };
    // o pico é o ponto mais longe do repouso (para baixo se o freio estiver montado ao contrário)
    let pico = if repouso.0 - menor > maior - repouso.1 { menor } else { maior };
    let Some(nova) = calibracao::sugerir(repouso, pico, &atual) else {
        eprintln!("\nO curso medido foi curto demais (repouso {repouso:?}, pico {pico}). Puxe o freio até o fim e tente de novo.");
        return 1;
    };

    println!("\nResultado:");
    println!("  min = {}   (antes: {})", nova.min, atual.min);
    println!("  max = {}   (antes: {})", nova.max, atual.max);
    println!("  inverter = {}   (antes: {})", nova.inverter, atual.inverter);
    println!("  curva = {}   (mantida; ajuste no arquivo se quiser)", nova.curva);

    println!("\n3) Teste: puxe o freio e veja a saída calibrada (o que o jogo vai ver). Aperte Enter para continuar.");
    teste_ao_vivo(&freio, &enter, &nova);

    if !perguntar(&enter, &format!("\nSalvar em {}? [S/n] ", caminho.display())) {
        println!("Nada foi gravado.");
        return 0;
    }
    match calibracao::salvar(&caminho, &nova) {
        Ok(()) => {
            println!("Gravado. O serviço do freio aplica sozinho em até meio segundo.");
            0
        }
        Err(e) => {
            eprintln!("Erro ao gravar {}: {e}", caminho.display());
            1
        }
    }
}

fn teste_ao_vivo(freio: &Receiver<i32>, enter: &Receiver<String>, c: &Calibracao) {
    let mut atual = None;
    loop {
        if let Ok(v) = freio.recv_timeout(Duration::from_millis(30)) {
            atual = Some(v);
        }
        if let Some(v) = atual {
            print!("\r  bruto {v:4}  ->  {}   ", barra(c.aplicar(v), 40));
            let _ = io::stdout().flush();
        }
        if enter.try_recv().is_ok() {
            println!();
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desenho_da_barra() {
        assert_eq!(barra(0, 10), "[..........]    0 / 1023");
        assert_eq!(barra(RAW_MAX, 10), "[##########] 1023 / 1023");
        assert_eq!(barra(512, 10), "[#####.....]  512 / 1023");
    }
}
