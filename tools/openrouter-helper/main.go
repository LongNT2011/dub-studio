// openrouter-helper — компилируемый сайдкар (Go SDK OpenRouter) для облачных вызовов dub-studio.
// Как llama-server/whisper/bsroformer: один статический бинарь, который дёргает Rust-бэкенд.
// Протокол: аргумент = операция, JSON на stdin, ключ из env OPENROUTER_API_KEY.
//   chat   {model, temperature, top_p, top_k, max_tokens, messages:[{role, content|parts}]} -> {content, cost}
//   tts    {model, input, voice, format, out}                                                -> {ok, cost}
//   stt    {model, audio_b64, format, language}                                              -> {text, cost}
//   models {output_modalities?, input_modalities?}                                           -> {models:[{id,name}]}
//   voices {model}                                                                            -> {voices:[...]}
//   verify {}                                                                                 -> {ok, credits}
package main

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"strings"

	openrouter "github.com/OpenRouterTeam/go-sdk"
	"github.com/OpenRouterTeam/go-sdk/models/components"
	"github.com/OpenRouterTeam/go-sdk/models/operations"
)

func die(err error) {
	if err != nil {
		fmt.Fprintln(os.Stderr, err.Error())
		os.Exit(1)
	}
}

func readIn() map[string]any {
	b, _ := io.ReadAll(os.Stdin)
	m := map[string]any{}
	if len(b) > 0 {
		_ = json.Unmarshal(b, &m)
	}
	return m
}

func out(v any) {
	b, _ := json.Marshal(v)
	os.Stdout.Write(b)
	os.Stdout.Write([]byte("\n"))
}

func str(m map[string]any, k string) string {
	if v, ok := m[k].(string); ok {
		return v
	}
	return ""
}

func main() {
	if len(os.Args) < 2 {
		die(fmt.Errorf("usage: openrouter-helper <chat|tts|stt|models|voices|verify>"))
	}
	key := os.Getenv("OPENROUTER_API_KEY")
	if key == "" {
		die(fmt.Errorf("OPENROUTER_API_KEY не задан"))
	}
	s := openrouter.New(openrouter.WithSecurity(key))
	ctx := context.Background()
	in := readIn()

	switch os.Args[1] {
	case "tts":
		// УНИВЕРСАЛЬНЫЙ формат — pcm (без потерь, 24кГц s16le mono; дефолт SDK). Rust всегда конвертит
		// pcm->wav. Мультиязычные модели (gemini и пр.) его отдают; per-model перебора нет.
		rf := components.SpeechRequestResponseFormatPcm
		req := components.SpeechRequest{
			Model:          str(in, "model"),
			Input:          str(in, "input"),
			Voice:          str(in, "voice"),
			ResponseFormat: &rf,
		}
		body, err := s.Tts.CreateSpeech(ctx, req)
		die(err)
		defer body.Close()
		f, err := os.Create(str(in, "out"))
		die(err)
		n, err := io.Copy(f, body)
		f.Close()
		die(err)
		out(map[string]any{"ok": true, "bytes": n})

	case "models":
		req := &operations.GetModelsRequest{}
		if v := str(in, "output_modalities"); v != "" {
			req.OutputModalities = &v
		}
		if v := str(in, "input_modalities"); v != "" {
			req.InputModalities = &v
		}
		res, err := s.Models.List(ctx, req)
		die(err)
		// Result -> сериализуем как есть, Rust вытащит id/name.
		raw, _ := json.Marshal(res.Result)
		os.Stdout.Write(raw)
		os.Stdout.Write([]byte("\n"))

	case "stt":
		// Облачная транскрипция: wav-файл -> base64 -> verbose_json с сегментами (start/end/text) для пайплайна.
		audioPath := str(in, "audio")
		raw, err := os.ReadFile(audioPath)
		die(err)
		rf := components.STTRequestResponseFormatVerboseJSON
		req := components.STTRequest{
			Model:                  str(in, "model"),
			InputAudio:             components.STTInputAudio{Data: base64.StdEncoding.EncodeToString(raw), Format: "wav"},
			ResponseFormat:         &rf,
			TimestampGranularities: []components.STTTimestampGranularity{components.STTTimestampGranularitySegment},
		}
		if lang := str(in, "language"); lang != "" {
			req.Language = &lang
		}
		res, err := s.Stt.CreateTranscription(ctx, req)
		die(err)
		out(map[string]any{"text": res.Text, "segments": res.Segments, "language": res.Language, "duration": res.Duration})

	case "voices":
		// Голоса конкретной TTS-модели (Models.Get -> supported_voices). id = "author/slug".
		id := str(in, "model")
		author, slug, ok := strings.Cut(id, "/")
		if !ok {
			die(fmt.Errorf("id модели без '/': %s", id))
		}
		res, err := s.Models.Get(ctx, author, slug)
		die(err)
		out(map[string]any{"voices": res.Data.SupportedVoices})

	case "verify":
		res, err := s.Credits.GetCredits(ctx)
		if err != nil {
			out(map[string]any{"ok": false, "error": err.Error()})
			return
		}
		raw, _ := json.Marshal(res)
		out(map[string]any{"ok": true, "credits": json.RawMessage(raw)})

	default:
		die(fmt.Errorf("неизвестная операция: %s", os.Args[1]))
	}
}
