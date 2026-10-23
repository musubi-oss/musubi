// One WebSocket to the node per simulator: `GET /v1/server/worlds/{w}/stream?sim=<uuid>&delta=1`
// authenticated with the fleet/server key. Frames in (`type: tick|error`), commands out as
// `SimCommandFrame` with an `id` the reply echoes. Reconnects with backoff; while connected the
// node renews the simulator's lease by itself.
#pragma once

#include "CoreMinimal.h"
#include "MusubiSimWire.generated.h"
#include "MusubiSimCommandFrame.h"

class IWebSocket;

enum class EMusubiSimStreamState : uint8 { Closed, Connecting, Open, Backoff };

DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiSimFrameEvent, const FMusubiSimFrameTick&);
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiSimStreamErrorEvent, uint64 /*tick*/, const FString& /*error*/);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiSimStreamStateEvent, EMusubiSimStreamState);
/** Reply to a command: `bOk` false on `error`/`redirect` (then `Reply["error"]`/`Reply["redirect"]` say why). */
typedef TFunction<void(bool bOk, const TSharedPtr<FJsonObject>& Reply)> FMusubiSimReply;

class MUSUBISERVER_API FMusubiSimStream : public TSharedFromThis<FMusubiSimStream>
{
public:
	struct FConfig
	{
		bool bDelta = true;
		double ReplyTimeoutSec = 10.0;
		double BackoffMinSec = 0.5, BackoffMaxSec = 10.0;
		bool bAutoReconnect = true;
	};

	~FMusubiSimStream();
	void Connect(const FString& ApiBase, const FString& ServerKey, const FString& World, const FMusubiUuid& SimId, const FConfig& InConfig);
	void Close();
	/** Drive timeouts and reconnects; call every engine tick. */
	void Tick(double DeltaSeconds);

	bool IsOpen() const { return State == EMusubiSimStreamState::Open; }
	EMusubiSimStreamState GetState() const { return State; }
	/** Queue a command; `OnReply` fires once with the node's answer (or a local timeout/closed error). Returns false when not connected. */
	bool Send(const FMusubiSimCommand& Cmd, FMusubiSimReply OnReply = nullptr);
	/** The node's current URL for a cell hosted elsewhere, learned from `redirect` replies. */
	const TMap<FString, FString>& Redirects() const { return RedirectCache; }

	FMusubiSimFrameEvent OnFrame;
	FMusubiSimStreamErrorEvent OnFrameError;
	FMusubiSimStreamStateEvent OnStateChanged;

	struct FStats { uint64 FramesIn = 0, CommandsOut = 0, RepliesIn = 0, Errors = 0, Reconnects = 0; double LastFrameAt = 0; };
	const FStats& GetStats() const { return Stats; }

private:
	struct FPending { FMusubiSimReply OnReply; double SentAt = 0; FString Op; };
	TSharedPtr<IWebSocket> Ws;
	FString Url, Key, WorldName;
	FMusubiUuid Sim;
	FConfig Config;
	EMusubiSimStreamState State = EMusubiSimStreamState::Closed;
	TMap<uint64, FPending> Pending;
	uint64 NextId = 1;
	double Now = 0, BackoffUntil = 0, Backoff = 0;
	TMap<FString, FString> RedirectCache;
	FStats Stats;

	void Open();
	void SetState(EMusubiSimStreamState S);
	void HandleText(const FString& Text);
	void FailPending(const FString& Why);
	void ScheduleReconnect();
};
