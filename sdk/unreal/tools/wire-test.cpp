// Golden-vector test for the generated wire layer, run against the UE shim:
//   for each vector {name, json, postcard(hex), kind: client|server}
//     JSON text  -> struct -> JSON   : parsed value equals the Rust JSON (field order aside)
//     JSON text  -> struct -> postcard: bytes equal Rust's postcard output
//     postcard   -> struct -> JSON   : parsed value equals the Rust JSON
// Vectors come from `cargo test -p musubi-proto --test wire_vectors` (writes wire-vectors.json).
#include "MusubiEnvelope.h"
#include "Serialization/JsonSerializer.h"
#include <fstream>
#include <sstream>
#include <iostream>

static std::string Hex(const TArray<uint8>& B)
{
	static const char* H = "0123456789abcdef";
	std::string S;
	for (uint8 X : B) { S.push_back(H[X >> 4]); S.push_back(H[X & 15]); }
	return S;
}
static TArray<uint8> Unhex(const std::string& S)
{
	TArray<uint8> B;
	for (size_t I = 0; I + 1 < S.size(); I += 2) B.Add((uint8)std::stoul(S.substr(I, 2), nullptr, 16));
	return B;
}
static bool Same(const TSharedPtr<FJsonValue>& A, const TSharedPtr<FJsonValue>& B)
{
	if (!A || !B) return !A && !B;
	if (A->IsNull() || B->IsNull()) return A->IsNull() && B->IsNull();
	if (A->Type != B->Type) return false;
	switch (A->Type)
	{
	case EJson::String: return A->Str == B->Str;
	case EJson::Number: return A->Num == B->Num;
	case EJson::Boolean: return A->B == B->B;
	case EJson::Array:
		if (A->Arr.Num() != B->Arr.Num()) return false;
		for (int32 I = 0; I < A->Arr.Num(); ++I) if (!Same(A->Arr[I], B->Arr[I])) return false;
		return true;
	case EJson::Object:
		if (A->Obj->Values.Num() != B->Obj->Values.Num()) return false;
		for (const auto& P : A->Obj->Values) { TSharedPtr<FJsonValue> O = B->Obj->TryGetField(P.Key); if (!O || !Same(P.Value, O)) return false; }
		return true;
	default: return true;
	}
}

template <typename TEnv>
static int Check(const std::string& Name, const std::string& Json, const std::string& PcHex, int& Failed)
{
	auto Fail = [&](const char* What, const std::string& Got) { std::cout << "FAIL " << Name << ": " << What << "\n  got  " << Got << "\n"; ++Failed; return 1; };
	TSharedPtr<FJsonValue> Want = MusubiJson::Deserialize(FString(Json));
	TEnv E;
	if (!E.FromJsonText(FString(Json))) return Fail("json decode", Json);
	FString Back = E.ToJsonText();
	if (!Same(Want, MusubiJson::Deserialize(Back))) return Fail("json roundtrip", Back.S + "\n  want " + Json);
	TArray<uint8> Pc = E.ToPostcard();
	if (Hex(Pc) != PcHex) return Fail("postcard encode", Hex(Pc) + "\n  want " + PcHex);
	TArray<uint8> In = Unhex(PcHex);
	TEnv F;
	if (!F.FromPostcard(In.GetData(), In.Num())) return Fail("postcard decode", PcHex);
	FString Back2 = F.ToJsonText();
	if (!Same(Want, MusubiJson::Deserialize(Back2))) return Fail("postcard->json", Back2.S + "\n  want " + Json);
	return 0;
}

int main(int Argc, char** Argv)
{
	const char* Path = Argc > 1 ? Argv[1] : "tools/wire-vectors.json";
	std::ifstream F(Path);
	if (!F) { std::cerr << "cannot open " << Path << "\n"; return 2; }
	std::stringstream SS; SS << F.rdbuf();
	TSharedPtr<FJsonValue> Root = MusubiJson::Deserialize(FString(SS.str()));
	const TArray<TSharedPtr<FJsonValue>>* Arr;
	if (!Root || !Root->TryGetArray(Arr)) { std::cerr << "bad vectors file\n"; return 2; }
	int Failed = 0, Total = 0;
	for (const TSharedPtr<FJsonValue>& V : *Arr)
	{
		const TSharedPtr<FJsonObject>* O; if (!V->TryGetObject(O)) continue;
		FString Name, Kind, Json, Pc;
		(*O)->TryGetStringField("name", Name); (*O)->TryGetStringField("kind", Kind);
		(*O)->TryGetStringField("json", Json); (*O)->TryGetStringField("postcard", Pc);
		++Total;
		if (Kind == "client") Check<FMusubiClientEnvelope>(Name.S, Json.S, Pc.S, Failed);
		else Check<FMusubiServerEnvelope>(Name.S, Json.S, Pc.S, Failed);
	}
	std::cout << (Total - Failed) << "/" << Total << " vectors ok\n";
	return Failed ? 1 : 0;
}
