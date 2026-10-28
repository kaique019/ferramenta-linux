# Pingu

Versão para Linux, em Rust, inspirada no ResetExplode (Windows).

**Status: em desenvolvimento.** Prontos até agora (com testes): utilidades, informações de sistema/CPU/RAM/monitores,
PCI, configurações com gravação atômica, sensores (hwmon/RAPL), GPU (NVIDIA/AMD/Intel) e discos com S.M.A.R.T.

Faltam: stress test, benchmarks, detector de gargalo, ferramentas (jogos, portas seriais, inicialização), TUI e CLI.

```
cargo test
```

## Aparência da TUI (theme.json)

As cores e símbolos da interface são configuráveis por JSON em `~/.config/pingu/theme.json`.

> **Fonte (tipo/tamanho da letra):** o programa roda dentro do terminal, então a fonte é do seu
> emulador de terminal (GNOME Terminal, Konsole, Alacritty, kitty...). O JSON controla cores,
> negrito/itálico/sublinhado, bordas e símbolos.

```sh
pingu theme --list                      # presets: padrao, claro, contraste, retro, neon
pingu theme --preset neon --write       # grava o preset como theme.json para editar
```

Campos: `accent`, `muted`, `ok`, `warn`, `err`, `selection_fg`, `selection_bg`, `border`, `border_focus`,
`gauge_bg`, `bold_headings`, `italic_muted`, `underline_selection`, `rounded_borders`, `pointer`,
`check_on`, `check_off`, `divider`. Cores: nome (`red`), `#rrggbb`, `#rgb` ou número 0–255.
Na TUI: `t` alterna presets (temporário), `T` recarrega o arquivo. Arquivo inválido volta ao padrão com aviso.

### Janela pequena

A TUI se ajusta sozinha ao tamanho do terminal: abaixo de 80 colunas os painéis lado a lado ficam
empilhados e os botões (GameMode, MangoHud...) passam a uma linha cada; abaixo de 24 linhas o cabeçalho
e o rodapé encolhem para uma linha e as abas mostram só o número; em terminais bem pequenos somem as
bordas e a aba Ferramentas mostra um painel por vez (←→ troca). O mínimo é 24x6. O tamanho da letra
continua sendo do terminal (Ctrl+- costuma diminuí-la), o programa não consegue mudá-lo.
