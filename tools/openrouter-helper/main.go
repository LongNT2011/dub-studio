// openrouter-helper — компилируемый сайдкар (Go SDK OpenRouter) для облачных вызовов dub-studio.
// Как llama-server/whisper/bsroformer: один статический бинарь, который дёргает Rust-бэкенд.
// Протокол: аргумент = операция, JSON на stdin, ключ из env OPENROUTER_API_KEY.
//   chat   {model, temperature, top_p, top_k, max_tokens, messages:[{role, content|parts}]} -> {content, cost}
//   tts    {model, input, voice, format, out}                                                -> {ok, cost}
//   stt    {model, audio_b64, format, language}                                              -> {text, cost}
//   models {output_modalities?, input_modalities?}                                           -> {models:[{id,name}]}
//   verify {}                                                                                 -> {ok, credits}
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"

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
		die(fmt.Errorf("usage: openrouter-helper <chat|tts|stt|models|verify>"))
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
		format := components.SpeechRequestResponseFormatMp3
		req := components.SpeechRequest{
			Model:          str(in, "model"),
			Input:          str(in, "input"),
			Voice:          str(in, "voice"),
			ResponseFormat: &format,
		}
		body, err := s.Tts.CreateSpeech(ctx, req)
		die(err)
		defer body.Close()
		outPath := str(in, "out")
		f, err := os.Create(outPath)
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
