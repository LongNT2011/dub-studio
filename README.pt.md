<div align="center">

<img src="frontend/public/favicon.svg" width="72" alt="Dub Studio"/>

# Dub Studio

**Estúdio de dublagem de vídeo com IA, gratuito e offline, para Windows —— redubla qualquer vídeo curto para outro idioma com voz clonada, legendas traduzidas e localização do texto na tela. 100% local, zero Python: um `.exe` nativo (Rust + C++/CUDA); todos os modelos e motores baixam com um botão.**

[![License](https://img.shields.io/github/license/timoncool/dub-studio?style=flat-square)](LICENSE)
[![Stars](https://img.shields.io/github/stars/timoncool/dub-studio?style=flat-square)](https://github.com/timoncool/dub-studio/stargazers)
[![Latest release](https://img.shields.io/github/v/release/timoncool/dub-studio?include_prereleases&style=flat-square)](https://github.com/timoncool/dub-studio/releases)
[![Downloads](https://img.shields.io/github/downloads/timoncool/dub-studio/total?style=flat-square)](https://github.com/timoncool/dub-studio/releases)

[English](README.md) · [Русский](README.ru.md) · [中文](README.zh.md) · [Español](README.es.md) · **Português** · [Français](README.fr.md)

### [🌐 Demo ao vivo e showcase antes/depois →](https://timoncool.github.io/dub-studio/)



</div>

## Veja em ação

**[▶ Ver o showcase antes/depois no site →](https://timoncool.github.io/dub-studio/#showcase)** — clipes reais dublados de ponta a ponta numa GPU local: vídeos, modos e idiomas diferentes.

| ![dub](docs/shots/mode-dub-ru.png) | ![voiceover](docs/shots/mode-voiceover-es.png) | ![transcript](docs/shots/mode-transcribe-fr.png) |
|:--:|:--:|:--:|
| 🎙️ **Dublagem** · EN→RU | 🗣️ **Locução** · EN→ES | 🎬 **Transcrição** · diarização |

## O que é

**Dub Studio** transforma qualquer vídeo curto em uma versão dublada para outro idioma —— **com o timbre do falante clonado, legendas traduzidas e o texto embutido localizado sobre o próprio quadro**. Solte um clipe e um passe automático inteligente cria o primeiro rascunho; depois um editor ao vivo coloca **cada legenda, voz, caixa de desfoque, fonte e título** sob seu controle com pré-visualização instantânea.

Tudo roda **100% na sua máquina** —— sem nuvem, sem upload, sem assinatura. Nem o seu material nem a sua voz saem do computador.

Esta é a **v2 —— uma reescrita totalmente nativa**. Sem Python embutido, sem torch, sem wheels CUDA. Todo o pipeline é **Rust + motores nativos C++/CUDA (GGUF/ONNX)**: um processo, início rápido, baixo consumo de VRAM. Modelos, motores, runtime CUDA/VC++ e ffmpeg são **baixados e instalados pelo próprio app** no primeiro uso —— o único passo manual é o driver NVIDIA.

## Cinco modos, alternáveis na hora

| Modo | O que faz |
|------|-----------|
| 🎙️ **Dublagem** | Redublagem completa para o idioma-alvo com o **timbre original clonado** —— elenco automático por falante ou voz à escolha |
| 🗣️ **Locução (voz sobreposta)** | Voz traduzida **sobre o original atenuado** —— o original ainda é ouvido embaixo; equilíbrio ajustável |
| 📝 **Legendas** | Legendas no **idioma original**, mantendo o áudio original —— sem dublagem nem tradução |
| ✨ **Remix engraçado** | Dê um tema («como um pirata», «como um telejornal») → o modelo **reescreve todo o roteiro** e redubla |
| 🎬 **Transcrição** | **Transcrição diarizada** limpa com layout por falante, reprodução tipo karaokê, criação de vozes com um clique, export `.srt`/`.txt` |

Carregue um clipe uma vez e envie-o para qualquer modo dentro do editor.

## Recursos

- **Clonagem de voz** —— o timbre original é clonado e fala o novo idioma (motor nativo [Higgs Audio v3](https://huggingface.co/bosonai), GGUF). Elenco automático por falante ou sua própria voz de um pack.
- **Diarização de falantes** —— quem fala e quando (NVIDIA **Sortformer** v2, até 4 vozes), uma voz distinta por falante.
- **Escolha do motor de ASR** — transcreva com **Parakeet-TDT** (GPU, padrão) ou **Whisper** ([faster-whisper standalone da Purfview](https://github.com/Purfview/whisper-standalone-win), roda na CPU) — escolha o tamanho do modelo (tiny … large-v3-turbo) e o quant (compute type) direto nas configurações.
- **Pipeline combinável** — interruptores independentes na entrada: áudio (original / dublagem / voz sobreposta / transcrição) × legendas (nenhuma / original / traduzidas) × gravar no vídeo sim/não × remix humorístico. Qualquer combinação — dublagem sem legendas, legendas traduzidas sem dublagem, dublagem humorística com suas próprias vozes — também no lote e no editor.
- **Localização de texto na tela** —— OCR detecta o texto embutido (**PP-OCR** ONNX), **desfoca o original** e imprime por cima um título localizado com estilo combinado —— um recurso que nenhuma outra ferramenta tem.
- **Tradução + análise visual de estilo** —— a transcrição é traduzida localmente com **Gemma-4 12B** (GGUF, llama.cpp); um passe de visão lê o layout do quadro: estilo de legenda, títulos, marcas, zonas de texto.
- **Separação vocal SOTA** —— **Mel-Band Roformer** (BSRoformer.cpp nativo em CUDA) separa voz de música: a faixa de fundo é **preservada** e o clone se prende à fala limpa.
- **26 predefinições de legenda** —— karaokê / palavra a palavra / hormozi / neon e mais, renderizadas **no seu quadro** (WYSIWYG, JASSUB sobre o mesmo `.ass` que o ffmpeg grava).
- **Transcrição karaokê** —— reproduza o vídeo e acompanhe a linha e a **palavra** atuais acendendo na transcrição.
- **Editor ao vivo** —— edite transcrição, vozes, estilo de legenda, caixas de desfoque, títulos; **prévia ~0,17 s/quadro**, cada mudança visível na hora.
- **Regeneração inteligente** —— ao exportar, só os segmentos alterados são ressintetizados, não o clipe inteiro.
- **Processamento em lote** —— fila de arquivos, todos com uma configuração, progresso por arquivo.
- **Comparar antes/depois** —— original e dublagem lado a lado.
- **6 idiomas** —— EN / RU / ZH / ES / PT / FR, com detecção automática do idioma de origem.
- **Qualquer formato de vídeo** —— MP4, MOV, MKV, WEBM, AVI e mais (decodificado via ffmpeg).
- **Instalação de um botão + atualização automática** —— modelos, motores, runtime CUDA/VC++ e ffmpeg baixam no primeiro uso; o app se atualiza sozinho.
- **Totalmente portátil** —— nada é escrito no seu perfil de usuário; apague a pasta e não sobra rastro.

## Capturas

Tela inicial —— cinco modos, prévia do vídeo escolhido, seleção de idioma, qualquer formato:

![Tela inicial do Dub Studio](docs/screenshot-home.png)

Modo transcrição —— transcrição diarizada com layout por falante, karaokê e criação de vozes de cada falante com um clique:

![Modo transcrição do Dub Studio](docs/screenshot-transcribe.png)

## Requisitos

- **SO:** Windows 10 / 11 (x64)
- **GPU:** NVIDIA com 8–16 GB de VRAM
- **WebView2** —— pré-instalado no Windows 11 (instala sozinho no Windows 10)
- **Disco:** ~15 GB para modelos, motores e runtime (baixados no primeiro uso), mais espaço para seus projetos

A única coisa que você instala à mão é um **[driver NVIDIA](https://www.nvidia.com/Download/index.aspx)** recente. Todo o resto o app baixa com um botão no primeiro uso.

## Início rápido

1. **Baixe** a versão portátil em [Releases](https://github.com/timoncool/dub-studio/releases) e descompacte onde quiser (ou instale via `-setup.exe` / `.msi`).
2. **Execute** `Dub Studio.exe`.
3. No painel de **primeiro uso** clique em **Baixar tudo** —— o app busca modelos, motores e runtime (~15 GB, uma vez).
4. **Solte um vídeo**, escolha o idioma-alvo → o passe automático cria o primeiro rascunho. Ajuste tudo no editor e clique em **Exportar**.

> Tudo baixa e fica **dentro da pasta do app**. Modelos, caches e projetos não vão para outro lugar.

## Como funciona

`analyze()` é um primeiro passe fixo: separação → ASR com tempos por palavra → diarização → tradução contextual + visão (estilo de legenda / títulos / marcas) → OCR (layout / caixas de desfoque). O resultado é um documento **Project** editável. Cada edição é um patch sobre ele com prévia ~0,17 s/quadro; a exportação só reexecuta **os estágios alterados**.

**Stack:** um shell nativo **Tauri 2 (Rust)** inicia `dub-server` (axum) em uma porta local e abre uma janela sobre a SPA —— React 19 + Vite + Tailwind + react-konva sobre JASSUB. Motores: Parakeet-TDT ou Whisper (ASR) · Sortformer (diarização) · Gemma-4-12B GGUF (tradução + visão, llama.cpp) · Higgs Audio v3 (TTS) · Mel-Band Roformer (separação, BSRoformer.cpp) · PP-OCR (ONNX) · ffmpeg/NVENC. **Nenhum processo Python em tempo de execução.**

### Compilar do código

```bash
git clone https://github.com/timoncool/dub-studio.git
cd dub-studio

cd frontend && npm install && npm run build && cd ..   # 1) SPA
cargo build --release -p dub-server                     # 2) servidor nativo (axum)
cd desktop && npm install && npx tauri build            # 3) shell de desktop (Tauri)
```

Requer Node 20+, Rust (toolchain MSVC) e WebView2. Os motores nativos não precisam ser recompilados —— o app baixa binários pré-compilados.

## Autores

- **Nerual Dreming** —— [Telegram](https://t.me/nerual_dreming) | [neuro-cartel.com](https://neuro-cartel.com) | fundador da [ArtGeneration.me](https://artgeneration.me)
- **Neuro-Soft** —— [Telegram](https://t.me/neuroport) | apps de IA portáteis

## Créditos

- **[Boson AI](https://huggingface.co/bosonai)** —— modelo Higgs Audio v3; **[drbaph / Higgs-Audio-v3-Studio](https://huggingface.co/drbaph/Higgs-Audio-v3-Studio)** —— quantizações GGUF e `audiocpp_engine.dll` nativo.
- **[NVIDIA Parakeet](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)** e **[Sortformer](https://huggingface.co/nvidia)** —— ASR e diarização; pesos ONNX de [istupakov/parakeet-tdt-0.6b-v3-onnx](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx) e [altunenes/parakeet-rs](https://github.com/altunenes/parakeet-rs).
- **[Google Gemma](https://huggingface.co/google/gemma-4-12b-it-qat-q4_0-gguf)** —— Gemma-4 12B (tradução + visão), via [llama.cpp](https://github.com/ggml-org/llama.cpp).
- **[chenmozhijin / BSRoformer.cpp](https://github.com/chenmozhijin/BSRoformer.cpp)** e **[GaboxR67](https://huggingface.co/GaboxR67)** —— o motor nativo e o modelo Mel-Band Roformer.

## Apoie o autor

Faço software de código aberto e pesquisa em IA —— a maior parte é de acesso livre. Doações me permitem criar e pesquisar mais.

**[Todas as formas de apoiar](DONATE.md)** | **[dalink.to/nerual_dreming](https://dalink.to/nerual_dreming)** | **[boosty.to/neuro_art](https://boosty.to/neuro_art)**

- **BTC:** `1E7dHL22RpyhJGVpcvKdbyZgksSYkYeEBC`
- **ETH (ERC20):** `0xb5db65adf478983186d4897ba92fe2c25c594a0c`
- **USDT (TRC20):** `TQST9Lp2TjK6FiVkn4fwfGUee7NmkxEE7C`

## Licença

O código do app é [MIT](LICENSE). Os pesos dos modelos mantêm suas licenças (Higgs Audio v3 —— Boson AI research/não comercial; Gemma —— Gemma Terms; etc.) —— auditados antes de cada lançamento.
