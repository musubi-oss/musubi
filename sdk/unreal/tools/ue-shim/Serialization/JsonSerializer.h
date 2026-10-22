// Tiny but complete JSON printer/parser (RFC 8259) so the shim can run golden-vector tests.
#pragma once
#include "Dom/JsonObject.h"
#include "Serialization/JsonWriter.h"
#include "Serialization/JsonReader.h"

struct FJsonSerializer
{
	static void WriteString(std::string& O, const std::string& S)
	{
		O.push_back('"');
		for (unsigned char C : S)
		{
			switch (C)
			{
			case '"': O += "\\\""; break;
			case '\\': O += "\\\\"; break;
			case '\n': O += "\\n"; break;
			case '\r': O += "\\r"; break;
			case '\t': O += "\\t"; break;
			case '\b': O += "\\b"; break;
			case '\f': O += "\\f"; break;
			default:
				if (C < 0x20) { char B[8]; std::snprintf(B, sizeof B, "\\u%04x", C); O += B; }
				else O.push_back((char)C);
			}
		}
		O.push_back('"');
	}
	static void WriteValue(std::string& O, const FJsonValue& V)
	{
		switch (V.Type)
		{
		case EJson::String: WriteString(O, V.Str.S); break;
		case EJson::Number:
		{
			char B[64];
			if (std::floor(V.Num) == V.Num && std::fabs(V.Num) < 1e15) std::snprintf(B, sizeof B, "%lld", (long long)V.Num);
			else std::snprintf(B, sizeof B, "%.17g", V.Num);
			O += B;
			break;
		}
		case EJson::Boolean: O += V.B ? "true" : "false"; break;
		case EJson::Array:
			O.push_back('[');
			for (int32 I = 0; I < V.Arr.Num(); ++I) { if (I) O.push_back(','); WriteValue(O, *V.Arr[I]); }
			O.push_back(']');
			break;
		case EJson::Object:
		{
			O.push_back('{');
			bool First = true;
			for (const auto& P : V.Obj->Values.M) { if (!First) O.push_back(','); First = false; WriteString(O, P.Key.S); O.push_back(':'); WriteValue(O, *P.Value); }
			O.push_back('}');
			break;
		}
		default: O += "null";
		}
	}
	template <typename W> static bool Serialize(const TSharedRef<FJsonObject>& O, const TSharedRef<W>& Wr) { FJsonValueObject V(O); WriteValue(Wr->Out->S, V); return true; }
	template <typename W> static bool Serialize(const TArray<TSharedPtr<FJsonValue>>& A, const TSharedRef<W>& Wr) { FJsonValueArray V(A); WriteValue(Wr->Out->S, V); return true; }

	struct FParser
	{
		const std::string& S; size_t P = 0; bool Err = false;
		explicit FParser(const std::string& In) : S(In) {}
		void Ws() { while (P < S.size() && (S[P] == ' ' || S[P] == '\n' || S[P] == '\r' || S[P] == '\t')) ++P; }
		bool Lit(const char* L) { size_t N = std::strlen(L); if (S.compare(P, N, L) == 0) { P += N; return true; } return false; }
		static void Utf8(std::string& O, unsigned CP)
		{
			if (CP < 0x80) O.push_back((char)CP);
			else if (CP < 0x800) { O.push_back((char)(0xC0 | (CP >> 6))); O.push_back((char)(0x80 | (CP & 0x3F))); }
			else if (CP < 0x10000) { O.push_back((char)(0xE0 | (CP >> 12))); O.push_back((char)(0x80 | ((CP >> 6) & 0x3F))); O.push_back((char)(0x80 | (CP & 0x3F))); }
			else { O.push_back((char)(0xF0 | (CP >> 18))); O.push_back((char)(0x80 | ((CP >> 12) & 0x3F))); O.push_back((char)(0x80 | ((CP >> 6) & 0x3F))); O.push_back((char)(0x80 | (CP & 0x3F))); }
		}
		bool Str(std::string& O)
		{
			if (P >= S.size() || S[P] != '"') return false;
			++P;
			while (P < S.size())
			{
				char C = S[P++];
				if (C == '"') return true;
				if (C != '\\') { O.push_back(C); continue; }
				if (P >= S.size()) return false;
				char E = S[P++];
				switch (E)
				{
				case '"': O.push_back('"'); break; case '\\': O.push_back('\\'); break; case '/': O.push_back('/'); break;
				case 'b': O.push_back('\b'); break; case 'f': O.push_back('\f'); break; case 'n': O.push_back('\n'); break;
				case 'r': O.push_back('\r'); break; case 't': O.push_back('\t'); break;
				case 'u':
				{
					if (P + 4 > S.size()) return false;
					unsigned CP = (unsigned)std::stoul(S.substr(P, 4), nullptr, 16); P += 4;
					if (CP >= 0xD800 && CP <= 0xDBFF && P + 6 <= S.size() && S[P] == '\\' && S[P + 1] == 'u')
					{
						unsigned Lo = (unsigned)std::stoul(S.substr(P + 2, 4), nullptr, 16); P += 6;
						CP = 0x10000 + ((CP - 0xD800) << 10) + (Lo - 0xDC00);
					}
					Utf8(O, CP);
					break;
				}
				default: return false;
				}
			}
			return false;
		}
		TSharedPtr<FJsonValue> Value()
		{
			Ws();
			if (P >= S.size()) { Err = true; return nullptr; }
			char C = S[P];
			if (C == '{')
			{
				++P; auto O = MakeShared<FJsonObject>(); Ws();
				if (P < S.size() && S[P] == '}') { ++P; return MakeShared<FJsonValueObject>(O); }
				while (true)
				{
					Ws(); std::string K; if (!Str(K)) { Err = true; return nullptr; }
					Ws(); if (P >= S.size() || S[P] != ':') { Err = true; return nullptr; } ++P;
					auto V = Value(); if (Err) return nullptr;
					O->SetField(FString(K), V);
					Ws(); if (P < S.size() && S[P] == ',') { ++P; continue; }
					if (P < S.size() && S[P] == '}') { ++P; break; }
					Err = true; return nullptr;
				}
				return MakeShared<FJsonValueObject>(O);
			}
			if (C == '[')
			{
				++P; TArray<TSharedPtr<FJsonValue>> A; Ws();
				if (P < S.size() && S[P] == ']') { ++P; return MakeShared<FJsonValueArray>(A); }
				while (true)
				{
					auto V = Value(); if (Err) return nullptr; A.Add(V);
					Ws(); if (P < S.size() && S[P] == ',') { ++P; continue; }
					if (P < S.size() && S[P] == ']') { ++P; break; }
					Err = true; return nullptr;
				}
				return MakeShared<FJsonValueArray>(A);
			}
			if (C == '"') { std::string O; if (!Str(O)) { Err = true; return nullptr; } return MakeShared<FJsonValueString>(FString(O)); }
			if (Lit("true")) return MakeShared<FJsonValueBoolean>(true);
			if (Lit("false")) return MakeShared<FJsonValueBoolean>(false);
			if (Lit("null")) return MakeShared<FJsonValueNull>();
			size_t E = P; char* End = nullptr; double D = std::strtod(S.c_str() + P, &End);
			if (End == S.c_str() + P) { Err = true; return nullptr; }
			P = (size_t)(End - S.c_str()); (void)E;
			return MakeShared<FJsonValueNumber>(D);
		}
	};
	template <typename R> static bool Deserialize(const TSharedRef<R>& Rd, TSharedPtr<FJsonValue>& Out)
	{
		FParser Pa(Rd->Text.S); Out = Pa.Value(); Pa.Ws();
		return !Pa.Err && Out && Pa.P == Pa.S.size();
	}
	template <typename R> static bool Deserialize(const TSharedRef<R>& Rd, TSharedPtr<FJsonObject>& Out)
	{
		TSharedPtr<FJsonValue> V; if (!Deserialize(Rd, V) || V->Type != EJson::Object) return false; Out = V->Obj; return true;
	}
	template <typename R> static bool Deserialize(const TSharedRef<R>& Rd, TArray<TSharedPtr<FJsonValue>>& Out)
	{
		TSharedPtr<FJsonValue> V; if (!Deserialize(Rd, V) || V->Type != EJson::Array) return false; Out = V->Arr; return true;
	}
};
