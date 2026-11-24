//! Calibração do freio de mão: transforma o valor bruto do sensor (0..=1023) no valor do eixo (0..=1023).
//!
//! Lida de `~/.config/handbrake.toml` (formato `chave = valor`, linhas com `#` são comentário) e relida ao vivo
//! quando o arquivo muda. Sem arquivo ou com valor inválido, vale o padrão (sem ajuste nenhum).
//!
//! ```toml
//! min = 40          # folga da mola: abaixo disso é 0%
//! max = 980         # ponto de bloqueio: a partir disso é 100% (antes do batente)
//! curva = 1.6       # 1.0 linear; > 1 começo mais suave (hidráulico); < 1 mais agressivo
//! inverter = false  # freio montado ao contrário
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const RAW_MAX: i32 = 1023;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibracao {
    pub min: i32,
    pub max: i32,
    pub curva: f64,
    pub inverter: bool,
}

impl Default for Calibracao {
    fn default() -> Self {
        Self { min: 0, max: RAW_MAX, curva: 1.0, inverter: false }
    }
}

impl Calibracao {
    /// Valor bruto do sensor -> valor do eixo (0..=RAW_MAX).
    pub fn aplicar(&self, raw: i32) -> i32 {
        let raw = raw.clamp(0, RAW_MAX);
        let raw = if self.inverter { RAW_MAX - raw } else { raw };
        if raw <= self.min {
            return 0;
        }
        if raw >= self.max {
            return RAW_MAX;
        }
        let t = (raw - self.min) as f64 / (self.max - self.min) as f64;
        (t.powf(self.curva) * RAW_MAX as f64).round().clamp(0.0, RAW_MAX as f64) as i32
    }

    /// Lê o texto `chave = valor`. Chave desconhecida é ignorada; valor inválido mantém o padrão.
    /// Devolve a calibração e os avisos (para o log).
    pub fn de_texto(texto: &str) -> (Self, Vec<String>) {
        let mut c = Self::default();
        let mut avisos = Vec::new();
        for (n, linha) in texto.lines().enumerate() {
            let linha = linha.split('#').next().unwrap_or("").trim();
            if linha.is_empty() {
                continue;
            }
            let Some((chave, valor)) = linha.split_once('=') else {
                avisos.push(format!("linha {}: esperado 'chave = valor'", n + 1));
                continue;
            };
            let (chave, valor) = (chave.trim(), valor.trim().trim_matches('"'));
            let ok = match chave {
                "min" => valor.parse().map(|v| c.min = v).is_ok(),
                "max" => valor.parse().map(|v| c.max = v).is_ok(),
                "curva" => valor.replace(',', ".").parse().map(|v| c.curva = v).is_ok(),
                "inverter" => valor.parse().map(|v| c.inverter = v).is_ok(),
                _ => {
                    avisos.push(format!("linha {}: chave desconhecida '{chave}'", n + 1));
                    true
                }
            };
            if !ok {
                avisos.push(format!("linha {}: valor inválido para '{chave}': {valor}", n + 1));
            }
        }
        // limites seguros: min < max dentro de 0..=1023, curva entre 0.2 e 5
        c.min = c.min.clamp(0, RAW_MAX - 1);
        c.max = c.max.clamp(c.min + 1, RAW_MAX);
        if !(0.2..=5.0).contains(&c.curva) || !c.curva.is_finite() {
            avisos.push(format!("curva {} fora de 0.2..5: usando 1.0", c.curva));
            c.curva = 1.0;
        }
        (c, avisos)
    }
}

/// Margem (em unidades brutas) somada ao repouso e tirada do pico: o ruído do sensor parado não vira freio e o
/// 100% chega antes do batente.
pub const MARGEM: i32 = 12;

/// Calibração sugerida a partir do que o sensor leu: `repouso` = faixa (menor, maior) com o freio solto, `pico` =
/// valor mais longe do repouso com o freio puxado. Mantém curva da calibração atual; detecta freio montado ao
/// contrário (puxar diminui o valor). `None` se o curso medido for curto demais (freio não foi puxado).
pub fn sugerir(repouso: (i32, i32), pico: i32, atual: &Calibracao) -> Option<Calibracao> {
    let inverter = pico < repouso.0;
    // no espaço já invertido, puxar sempre aumenta o valor
    let (rep, pico) = if inverter { (RAW_MAX - repouso.0, RAW_MAX - pico) } else { (repouso.1, pico) };
    let min = (rep + MARGEM).clamp(0, RAW_MAX - 1);
    let max = (pico - MARGEM).clamp(0, RAW_MAX);
    if max - min < 100 {
        return None;
    }
    Some(Calibracao { min, max, curva: atual.curva, inverter })
}

/// Texto do `handbrake.toml` (com as explicações).
pub fn para_texto(c: &Calibracao) -> String {
    format!(
        "# Calibração do freio de mão (gerada por `usb-handbrake-bridge --calibrar`).\n\
         # O serviço relê este arquivo sozinho quando ele muda.\n\
         min = {}          # folga da mola: abaixo disso é 0%\n\
         max = {}         # ponto de bloqueio: a partir disso é 100% (antes do batente)\n\
         curva = {}       # 1.0 linear; > 1 começo mais suave (hidráulico); < 1 mais agressivo\n\
         inverter = {}  # freio montado ao contrário\n",
        c.min, c.max, c.curva, c.inverter
    )
}

/// Grava sem deixar o arquivo pela metade (o serviço pode ler no meio).
pub fn salvar(caminho: &Path, c: &Calibracao) -> std::io::Result<()> {
    if let Some(dir) = caminho.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = caminho.with_extension("toml.tmp");
    fs::write(&tmp, para_texto(c))?;
    fs::rename(tmp, caminho)
}

/// `~/.config/handbrake.toml` (ou `$XDG_CONFIG_HOME/handbrake.toml`).
pub fn caminho_padrao() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("handbrake.toml")
}

/// Calibração que se recarrega sozinha quando o arquivo muda (data de modificação).
pub struct CalibracaoAoVivo {
    caminho: PathBuf,
    modificado: Option<SystemTime>,
    pub atual: Calibracao,
}

impl CalibracaoAoVivo {
    pub fn new(caminho: &Path) -> Self {
        let mut c = Self { caminho: caminho.to_path_buf(), modificado: None, atual: Calibracao::default() };
        c.recarregar_se_mudou();
        c
    }

    /// Relê o arquivo se ele mudou (ou sumiu). Devolve `true` se a calibração mudou.
    pub fn recarregar_se_mudou(&mut self) -> bool {
        let modificado = fs::metadata(&self.caminho).and_then(|m| m.modified()).ok();
        if modificado == self.modificado {
            return false;
        }
        self.modificado = modificado;
        let nova = match fs::read_to_string(&self.caminho) {
            Ok(texto) => {
                let (c, avisos) = Calibracao::de_texto(&texto);
                for a in avisos {
                    eprintln!("[calibração] {}: {a}", self.caminho.display());
                }
                c
            }
            Err(_) => Calibracao::default(),
        };
        let mudou = nova != self.atual;
        if mudou {
            println!(
                "[calibração] min={} max={} curva={} inverter={}",
                nova.min, nova.max, nova.curva, nova.inverter
            );
        }
        self.atual = nova;
        mudou
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padrao_nao_muda_nada() {
        let c = Calibracao::default();
        for raw in [0, 1, 500, 1022, 1023] {
            assert_eq!(c.aplicar(raw), raw);
        }
    }

    #[test]
    fn deadzones_e_curva() {
        let c = Calibracao { min: 100, max: 900, curva: 1.0, inverter: false };
        assert_eq!(c.aplicar(50), 0, "folga da mola");
        assert_eq!(c.aplicar(100), 0);
        assert_eq!(c.aplicar(500), 512, "meio do curso = metade");
        assert_eq!(c.aplicar(900), 1023, "ponto de bloqueio");
        assert_eq!(c.aplicar(1000), 1023);
        let suave = Calibracao { curva: 2.0, ..c };
        assert!(suave.aplicar(500) < 300, "curva > 1: começo mais suave");
        assert_eq!(suave.aplicar(900), 1023);
        let inv = Calibracao { inverter: true, ..Calibracao::default() };
        assert_eq!(inv.aplicar(0), 1023);
        assert_eq!(inv.aplicar(1023), 0);
    }

    #[test]
    fn leitura_do_arquivo() {
        let (c, avisos) = Calibracao::de_texto("# meu freio\nmin = 40\nmax = 980  # batente\ncurva = 1,6\ninverter = true\n");
        assert_eq!(c, Calibracao { min: 40, max: 980, curva: 1.6, inverter: true });
        assert!(avisos.is_empty());
        let (c, avisos) = Calibracao::de_texto("min = abc\nmax = 5000\ncurva = 99\nfoo = 1\nlixo");
        assert_eq!((c.min, c.max, c.curva), (0, 1023, 1.0));
        assert_eq!(avisos.len(), 4);
        let (c, _) = Calibracao::de_texto("min = 900\nmax = 100");
        assert!(c.min < c.max, "max sempre acima do min");
    }

    #[test]
    fn sugestao_da_calibracao() {
        let base = Calibracao { curva: 1.6, ..Calibracao::default() };
        let c = sugerir((30, 41), 990, &base).unwrap();
        assert_eq!(c, Calibracao { min: 53, max: 978, curva: 1.6, inverter: false });
        let inv = sugerir((980, 1000), 40, &base).unwrap();
        assert!(inv.inverter);
        assert_eq!((inv.min, inv.max), (43 + MARGEM, 983 - MARGEM));
        assert_eq!(inv.aplicar(990), 0, "solto = 0%");
        assert_eq!(inv.aplicar(40), RAW_MAX, "puxado = 100%");
        assert_eq!(sugerir((30, 41), 120, &base), None, "freio não foi puxado");
        let (lido, avisos) = Calibracao::de_texto(&para_texto(&c));
        assert_eq!(lido, c);
        assert!(avisos.is_empty());
    }

    #[test]
    fn recarrega_quando_o_arquivo_muda() {
        let dir = std::env::temp_dir().join(format!("handbrake-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("handbrake.toml");
        let mut ao_vivo = CalibracaoAoVivo::new(&p);
        assert_eq!(ao_vivo.atual, Calibracao::default(), "sem arquivo: padrão");
        fs::write(&p, "min = 50\n").unwrap();
        assert!(ao_vivo.recarregar_se_mudou());
        assert_eq!(ao_vivo.atual.min, 50);
        assert!(!ao_vivo.recarregar_se_mudou(), "sem mudança: não recarrega");
        fs::remove_file(&p).unwrap();
        assert!(ao_vivo.recarregar_se_mudou(), "arquivo apagado: volta ao padrão");
        assert_eq!(ao_vivo.atual, Calibracao::default());
        let _ = fs::remove_dir_all(dir);
    }
}
