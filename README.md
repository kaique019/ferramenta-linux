# Pingu 🐧

Ferramentas de hardware e de jogos para Linux, em Rust, com **interface de terminal (TUI) colorida** e **linha de comando**.
Mostra informações do PC, sensores, saúde dos discos, roda benchmarks e testes de estabilidade, gerencia jogos
(Steam, Heroic/Epic/GOG, Lutris) e aplica ajustes de desempenho **sempre com opção de desfazer**.

Inspirado no [ResetExplode](https://github.com/GustavoReset/ResetExplode) (Windows).

## Começando

```sh
git clone git@github.com:kaique019/ferramenta-linux.git
cd ferramenta-linux
cargo build --release
./target/release/pingu            # abre a TUI
./target/release/pingu --help     # lista os comandos
```

Precisa do Rust 1.88 ou mais novo. Quase tudo funciona sem root; o que precisa de `sudo` está marcado abaixo.
Em qualquer comando, `--json` troca a saída por JSON (bom para scripts).

> **Sudo é seguro com os seus arquivos:** quando você roda com `sudo`, o Pingu lê e grava as configurações, backups
> e jogos **como o seu usuário**, não como root, e o jogo iniciado por ele também roda como você.

---

## Interface (TUI)

Rode `pingu` sem argumentos. Abas: **1 Resumo · 2 Sensores · 3 Discos · 4 GPU · 5 Energia e rede · 6 Testes · 7 Jogos · 8 Ferramentas · 9 Processos**.

| Tecla | O que faz |
|---|---|
| `Tab` / `Shift+Tab`, `←` `→`, `1`–`9` | trocam de aba |
| `↑` `↓`, `PgUp` `PgDn`, `Home` `End` | rolam ou escolhem |
| `Enter` | inicia o teste, abre o jogo, aplica/desfaz o ajuste ou instala o programa |
| `Esc` / `Ctrl+C` | cancelam o teste em andamento (`Ctrl+C` sem teste sai) |
| `t` / `T` | `t` troca o tema (temporário), `T` recarrega o `theme.json` |
| `?` ou `F1` | ajuda dentro do programa |
| `q` | sai |

Mouse: clique em abas, listas e botões; duplo clique inicia um teste ou jogo; a roda rola.
A TUI se ajusta sozinha ao tamanho do terminal (abaixo de 80 colunas os painéis empilham; em janelas
pequenas somem as bordas; mínimo 24x6).

---

## O que cada função faz

### Informações do sistema

| Comando | Para que serve |
|---|---|
| `pingu info` | Resumo do PC: distribuição, kernel, placa-mãe, CPU (modelo, núcleos, threads), memória e **telas** (tamanho em polegadas, resolução e taxa de atualização lidas do monitor). Avisa se está numa máquina virtual. |
| `pingu sensors` | Temperaturas, rotação das ventoinhas, tensões, corrente, frequências e **consumo de energia** (CPU via RAPL, placa-mãe via hwmon). Ajuda a ver se o PC esquenta demais. |
| `pingu gpu` | Placas de vídeo **NVIDIA, AMD e Intel**: driver, VRAM usada/total, uso, temperatura, consumo (com o limite), clock do núcleo e da memória. |
| `pingu disks` | Discos (modelo, tamanho, SSD/HDD/NVMe, barramento, temperatura) e volumes montados com uso. Com `--smart`, lê o **S.M.A.R.T.** e dá um veredito de saúde (precisa do `smartctl` e de `sudo`). |
| `pingu pci` | Lista os dispositivos PCI (placa de vídeo, rede, controladoras...), no estilo `lspci`. |
| `pingu net` | Interfaces de rede (tipo, estado, velocidade, driver) e DNS. Com `--test`: **latência**, **velocidade de download** e **bufferbloat** (quanto a latência piora quando a conexão está cheia — causa de lag em jogos online). Usa a internet. |
| `pingu errors` | Procura no log do kernel por **erros de hardware**: CPU (MCE), memória RAM (EDAC), PCIe, reinício do driver de vídeo, erros de disco/NVMe, limitação por temperatura e falta de memória. É o equivalente Linux dos eventos WHEA do Windows. `--minutes N` limita ao período recente. Pode exigir estar no grupo `adm`/`systemd-journal` ou usar `sudo`. |
| `pingu serial` | Portas seriais USB (Arduino, ESP32, conversores CH340/FTDI) e **por que não abrem**: usuário fora do grupo `dialout`/`uucp`, `brltty` roubando o dispositivo ou `ModemManager` sondando a porta, com a correção sugerida. |
| `pingu report` | Gera um **relatório HTML** do sistema (`--out arquivo`, `--smart` inclui o SMART; com `--json` sai em JSON). Bom para anexar num pedido de ajuda. |

### Energia da CPU: `pingu power`

Mostra bateria (carga, saúde, ciclos), se está na tomada, o governador da CPU, o boost e o perfil de energia. As opções abaixo **mudam** esses valores e precisam de `sudo`:

| Opção | O que é e para que serve |
|---|---|
| `--governor <nome>` | O **governador** (*governor*) decide como a CPU ajusta a frequência. **`performance`** mantém a CPU sempre na frequência alta: o jogo responde mais rápido e tem menos quedas de FPS por a CPU "demorar a acordar", em troca de mais consumo, calor e ventoinha. **`powersave`** deixa a frequência baixa para economizar energia (ideal no dia a dia e em notebook na bateria). Outros comuns: `schedutil` e `ondemand` (sobem a frequência conforme a carga). A lista aceita depende do driver da sua CPU. |
| `--boost on/off` | Liga ou desliga o **turbo** (frequências acima da nominal). Ligado = mais desempenho e mais calor; desligado = mais frio e silencioso, útil para testar a estabilidade ou reduzir temperatura. |
| `--epp <valor>` | **EPP** (*Energy Performance Preference*): dica de energia versus desempenho para CPUs Intel/AMD modernas (`performance`, `balance_performance`, `balance_power`, `power`). Funciona junto do governador. |
| `--profile <perfil>` | **Perfil de plataforma** do firmware/notebook: `low-power`, `balanced` ou `performance`. Muda limites de energia e ventoinhas do aparelho todo, não só da CPU. |

> O jogo pode pedir o governador `performance` só enquanto roda — veja "Perfis de jogo" abaixo. Ao terminar, o Pingu devolve o governador original de cada núcleo.

### Jogos: `pingu games`

O Pingu encontra jogos da **Steam** (lê os `appmanifest` de todas as bibliotecas), do **Heroic** (Epic, GOG e avulsos; instalação normal, Flatpak e Snap), do **Lutris** e os que você adicionar à mão.

| Comando | Para que serve |
|---|---|
| `games list` | Lista os jogos encontrados e a origem de cada um. |
| `games profile <jogo>` | Mostra o perfil de desempenho do jogo. |
| `games set <jogo> ...` | Altera o perfil (só o que você informar). Veja a tabela abaixo. |
| `games launch <jogo>` | Abre o jogo com o perfil. **Steam**: o Steam não aceita opções de fora, então o Pingu mostra a linha para colar em *Propriedades → Opções de inicialização*. **Heroic (Epic/GOG/avulsos)**: abre pelo próprio Heroic (`heroic://launch/...`), que cuida do Wine/Proton. |
| `games add "Nome" /caminho/do/executavel` | Adiciona um jogo manual (`--args` para argumentos fixos). |

Dá para escolher o jogo pelo nome (mínimo 3 letras para busca parcial) ou pelo id mostrado em `games list`.

**O que cada opção do perfil faz** (`games set`, ou as teclas na aba Jogos):

| Opção (tecla) | O que faz |
|---|---|
| `--gamemode` (`g`) | Abre o jogo com o **GameMode** (Feral): ele sobe o governador da CPU para `performance`, ajusta a prioridade de E/S e pede à GPU que não reduza o clock enquanto o jogo roda; ao fechar, tudo volta. Precisa do pacote `gamemode`. |
| `--mangohud` (`m`) | Mostra o **overlay de FPS** (MangoHud) por cima do jogo: FPS, temperaturas, uso de CPU/GPU. Precisa do pacote `mangohud`. |
| `--dgpu` (`d`) | Força a **GPU dedicada** em notebooks com duas placas (PRIME): variáveis `__NV_PRIME_RENDER_OFFLOAD` para NVIDIA e `DRI_PRIME=1` para AMD. |
| `--performance-governor` (`p`) | Coloca a CPU em **governador `performance`** só enquanto o jogo roda e restaura o original de cada núcleo depois (precisa de `sudo`). Explicação do governador em "Energia da CPU". |
| `--priority` (`i`) | Prioridade do processo: `normal`, `above` (nice −5) ou `high` (nice −10). Dá preferência de CPU ao jogo; prioridade acima do normal **exige root**, senão é ignorada com aviso. |
| `--style`, `--res`, `--win-mode`, `--hz` | Resolução, modo de janela (`full`, `borderless`, `window`) e taxa de atualização, traduzidos para as opções de cada motor: `source`, `unity` ou `unreal`. |
| `--extra` | Argumentos extras para o jogo (ex.: `-novid`). Aceita aspas como no shell. |
| `--env` | Variáveis de ambiente (`CHAVE=valor;OUTRA=valor`). |

Os perfis ficam em `~/.config/pingu/settings.json`. O jogo é iniciado **sem passar por shell**, com a entrada padrão fechada, e se você cancelar (`Ctrl+C`), toda a árvore de processos é encerrada.

### Ajustes de sistema: `pingu tweaks`

Mudanças no kernel que ajudam jogos. **Valem só até reiniciar** e guardam o valor original para você desfazer (`tweaks restore`). Precisam de `sudo`.

| Ajuste | Valor | Por que ajuda |
|---|---|---|
| `swappiness` | 10 | O kernel usa menos o swap (disco) e mantém o jogo na RAM. |
| `max_map_count` | 1048576 | Muitos jogos via Proton/Wine (e o Steam Deck) exigem esse limite alto; sem ele podem travar ao abrir. |
| `vfs_cache_pressure` | 50 | Mantém por mais tempo os metadados de arquivos em cache, o que acelera o carregamento de mapas. |
| `thp` | `madvise` | *Transparent Hugepages* só para quem pede; evita travadinhas por compactação de memória. |

Comandos: `tweaks list` (estado de cada um), `tweaks apply <id>`, `tweaks restore <id>`. O restore se recusa a gravar valores adulterados e não reaplica o valor antigo se o PC reiniciou desde então.

### Programas de inicialização: `pingu startup`

Lista o que abre junto com a sessão (`~/.config/autostart` e do sistema), **desativa** um item guardando backup e **restaura** depois (`startup list`, `disable <nome>`, `removed`, `restore [nome]`). Acelera o login sem apagar nada de verdade.

### Instalar utilitários: `pingu install`

Instala, pelo gerenciador da sua distro (apt, dnf, pacman ou zypper), programas que o Pingu usa. Sem argumento lista o catálogo e o que já está instalado.

| Id | Para que serve |
|---|---|
| `mangohud` | Overlay de FPS nos jogos |
| `gamemode` | Otimizações automáticas enquanto o jogo roda |
| `lm-sensors` | Temperaturas e ventoinhas |
| `smartmontools` | Saúde dos discos (S.M.A.R.T.) |
| `pciutils` | `lspci` |
| `dmidecode` | Detalhes dos módulos de memória |
| `nvtop` | Monitor de GPU no terminal |
| `btop` | Monitor do sistema no terminal |

### Overlay de FPS: `pingu overlay`

Monta a configuração do **MangoHud** sem você editar arquivo: `--corner` (TL/TR/BL/BR), `--scale` (0,5 a 3), `--items fps,cpu_temp,gpu_load,...`. Sem `--write` só mostra o resultado; com `--write` grava em `~/.config/MangoHud/MangoHud.conf` (guardando `.bak`).

### Benchmarks: `pingu bench`

Todos rodam no próprio Pingu, sem programas externos, e, na TUI, os resultados são salvos para comparar depois.

| Comando | O que mede |
|---|---|
| `bench render` | Renderização (*path tracer* em todos os núcleos): desempenho da CPU em carga pesada. |
| `bench sim` | Simulação de jogo (30 mil unidades com física, colisão e IA): **FPS médio e 1% low** (os piores momentos, que é o que você sente como travada). |
| `bench memory` | Latência de acesso aleatório e largura de banda (leitura e cópia) da RAM. |
| `bench disk` | Velocidade sequencial e de blocos de 4 KiB, com E/S direta (sem cache). Cria e apaga arquivos temporários. |
| `bench load` | Carregamento de mapa simulado: um pacote grande mais mil arquivos pequenos. |

### Testes de estabilidade

| Comando | Para que serve |
|---|---|
| `pingu stress` | Carrega todos os núcleos (`--no-cpu` desliga) e opcionalmente a RAM (`--ram`, `--ram-fraction`). **Para sozinho** ao passar de `--max-temp` (padrão 90 °C). Mostra temperatura, carga, consumo e erros de RAM por segundo, e dá o veredito aprovado/não aprovado. Atenção: esquenta o PC e sobe as ventoinhas. |
| `pingu oc` | Verificador para quem fez **overclock ou undervolt**: estressa CPU e RAM por `--minutes` (padrão 10) e confere se o kernel registrou erros de hardware durante o teste. Se registrou, o ajuste não está estável. |

### Aba Processos

Na TUI, a aba **Processos** lista o que mais usa CPU ou memória (`c` ordena por CPU, `m` por memória).

---

## Aparência da TUI (theme.json)

As cores e símbolos são configuráveis por JSON em `~/.config/pingu/theme.json`.

> **Fonte (tipo/tamanho da letra):** o programa roda dentro do terminal, então a fonte é do seu emulador de terminal
> (GNOME Terminal, Konsole, Alacritty, kitty...). O JSON controla cores, negrito/itálico/sublinhado, bordas e símbolos.

```sh
pingu theme --list                      # padrao, claro, contraste, retro, neon, oceano, pinguim, dracula
pingu theme --preset neon --write       # grava o preset como theme.json para editar
```

Campos: `accent`, `muted`, `ok`, `warn`, `err`, `selection_fg`, `selection_bg`, `border`, `border_focus`, `gauge_bg`,
`bold_headings`, `italic_muted`, `underline_selection`, `rounded_borders`, `pointer`, `check_on`, `check_off`, `divider`.
Cores: nome (`red`), `#rrggbb`, `#rgb` ou número 0–255. Arquivo inválido volta ao padrão com aviso na barra de status.

## Limitações conhecidas

- Sensores, GPU e SMART dependem do hardware e dos drivers; em máquina virtual muitos não existem (o Pingu avisa).
- Se o Heroic já estiver aberto, GameMode/MangoHud/prioridade do perfil podem não chegar ao jogo, porque o Heroic repassa o pedido à instância em execução.
- O módulo de detecção de gargalo (o que limita o FPS: CPU, GPU, VRAM, RAM ou temperatura) existe, mas ainda não está ligado à interface.

## Desenvolvimento

```sh
cargo test      # a suíte usa uma árvore /sys e /proc falsa; nada depende do seu hardware
cargo clippy --all-targets
```
