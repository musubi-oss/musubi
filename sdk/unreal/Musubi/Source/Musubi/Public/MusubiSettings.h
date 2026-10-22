#pragma once

#include "CoreMinimal.h"
#include "Engine/DeveloperSettings.h"
#include "MusubiSocket.h"
#include "MusubiSettings.generated.h"

/**
 * Project Settings → Plugins → Musubi. Defaults for UMusubiSubsystem: node address and realtime socket
 * tuning. `Configure(BaseUrl)` at runtime overrides the address; `-MusubiApi=<url>` on the command line
 * overrides both. Saved to DefaultGame.ini under [/Script/Musubi.MusubiSettings].
 */
/** UENUM twins of the plain C++ enums in MusubiEnvelope.h / MusubiTransport.h (those headers are shim-compiled, no UHT). */
UENUM()
enum class EMusubiSettingsEncoding : uint8
{
	Json,
	Postcard,
};

UENUM()
enum class EMusubiSettingsTransport : uint8
{
	Auto,
	WebSocket,
	Quic,
};

UCLASS(Config = Game, DefaultConfig, meta = (DisplayName = "Musubi"))
class MUSUBI_API UMusubiSettings : public UDeveloperSettings
{
	GENERATED_BODY()
public:
	virtual FName GetCategoryName() const override { return TEXT("Plugins"); }

	/** Node base URL, e.g. http://127.0.0.1:8080. Empty → the game must call Configure(). */
	UPROPERTY(Config, EditAnywhere, Category = "Node") FString ApiBase = TEXT("http://127.0.0.1:8080");
	/** Wire encoding of the realtime socket. Postcard is the compact binary format the node prefers. */
	UPROPERTY(Config, EditAnywhere, Category = "Realtime") EMusubiSettingsEncoding Encoding = EMusubiSettingsEncoding::Json;
	/** Auto = QUIC when the MusubiQuic module has a native library for this platform, WebSocket otherwise. */
	UPROPERTY(Config, EditAnywhere, Category = "Realtime") EMusubiSettingsTransport Transport = EMusubiSettingsTransport::Auto;
	/** UDP port of the node's `quic.listen` (0 = same port as the HTTP base). */
	UPROPERTY(Config, EditAnywhere, Category = "Realtime", meta = (ClampMin = "0", ClampMax = "65535")) int32 QuicPort = 0;
	/** Accept the node's self-signed development certificate. Never in shipping. */
	UPROPERTY(Config, EditAnywhere, Category = "Realtime") bool bQuicInsecure = false;
	/** Ask the node to park the session on disconnect and resume it on reconnect (subscriptions survive). */
	UPROPERTY(Config, EditAnywhere, Category = "Realtime") bool bResumable = true;
	UPROPERTY(Config, EditAnywhere, Category = "Realtime") bool bAutoReconnect = true;
	UPROPERTY(Config, EditAnywhere, Category = "Realtime", meta = (ClampMin = "0.5")) float ReplyTimeoutSec = 8.f;
	UPROPERTY(Config, EditAnywhere, Category = "Realtime", meta = (ClampMin = "1")) float PingIntervalSec = 15.f;
	UPROPERTY(Config, EditAnywhere, Category = "Realtime", meta = (ClampMin = "0.05")) float BackoffMinSec = 0.25f;
	UPROPERTY(Config, EditAnywhere, Category = "Realtime", meta = (ClampMin = "0.5")) float BackoffMaxSec = 8.f;
	/** Requests in flight before Request() fails locally with `busy`. */
	UPROPERTY(Config, EditAnywhere, Category = "Realtime", meta = (ClampMin = "1")) int32 MaxInflight = 64;
	/** Largest frame accepted from the node (bytes). */
	UPROPERTY(Config, EditAnywhere, Category = "Realtime", meta = (ClampMin = "65536")) int32 MaxFrameBytes = 4 * 1024 * 1024;

	/** Effective node address: command line `-MusubiApi=` wins over the config value. */
	FString EffectiveApiBase() const;
	FMusubiSocket::FConfig SocketConfig() const;
};
