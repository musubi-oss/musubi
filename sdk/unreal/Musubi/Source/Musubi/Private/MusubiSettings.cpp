#include "MusubiSettings.h"
#include "Misc/CommandLine.h"
#include "Misc/Parse.h"

FString UMusubiSettings::EffectiveApiBase() const
{
	FString Cmd;
	if (FParse::Value(FCommandLine::Get(), TEXT("MusubiApi="), Cmd) && !Cmd.IsEmpty()) return Cmd;
	return ApiBase;
}

FMusubiSocket::FConfig UMusubiSettings::SocketConfig() const
{
	FMusubiSocket::FConfig C;
	C.Encoding = Encoding == EMusubiSettingsEncoding::Postcard ? EMusubiEncoding::Postcard : EMusubiEncoding::Json;
	C.Transport = Transport == EMusubiSettingsTransport::WebSocket ? EMusubiTransport::WebSocket : Transport == EMusubiSettingsTransport::Quic ? EMusubiTransport::Quic : EMusubiTransport::Auto;
	C.QuicPort = QuicPort;
	C.bQuicInsecure = bQuicInsecure;
	C.bResumable = bResumable;
	C.bAutoReconnect = bAutoReconnect;
	C.ReplyTimeoutSec = ReplyTimeoutSec;
	C.PingIntervalSec = PingIntervalSec;
	C.BackoffMinSec = BackoffMinSec;
	C.BackoffMaxSec = BackoffMaxSec;
	C.MaxInflight = MaxInflight;
	C.MaxFrameBytes = MaxFrameBytes;
	return C;
}
