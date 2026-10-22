// Minimal stand-in for Unreal's Core API so the generated wire layer can be syntax-checked
// (and unit-tested against musubi-proto golden vectors) with a plain C++17 compiler.
// Only what the Musubi wire layer uses; semantics match UE where it matters (UTF-8 <-> TCHAR,
// TOptional, TMap iteration, TSharedPtr), not performance.
#pragma once
#include <cstdint>
#include <cstring>
#include <cmath>
#include <string>
#include <vector>
#include <map>
#include <memory>
#include <optional>
#include <utility>
#include <algorithm>
#include <cstdio>
#include <climits>

typedef uint8_t uint8; typedef uint16_t uint16; typedef uint32_t uint32; typedef uint64_t uint64;
typedef int8_t int8; typedef int16_t int16; typedef int32_t int32; typedef int64_t int64;
typedef char ANSICHAR; typedef char TCHAR; typedef size_t SIZE_T;
#define TEXT(x) x
#define MUSUBI_API
#define UE_LOG(Cat, Verb, Fmt, ...) ((void)0)
#define DECLARE_LOG_CATEGORY_EXTERN(Cat, D, V) struct Cat##_t {};
#define DEFINE_LOG_CATEGORY(Cat)
#define check(x) ((void)0)
#define INT32_MIN_UE INT32_MIN

template <typename T> T&& MoveTemp(T& V) { return static_cast<T&&>(V); }
template <typename T> T&& MoveTemp(T&& V) { return static_cast<T&&>(V); }

struct FMemory
{
	static void Memcpy(void* D, const void* S, size_t N) { std::memcpy(D, S, N); }
	static int Memcmp(const void* A, const void* B, size_t N) { return std::memcmp(A, B, N); }
};
struct FMath
{
	template <typename T> static T Clamp(T V, T Lo, T Hi) { return V < Lo ? Lo : (V > Hi ? Hi : V); }
	static double RoundToDouble(double V) { return std::round(V); }
	static double Min(double A, double B) { return A < B ? A : B; }
	static float FRand() { return (float)std::rand() / (float)RAND_MAX; }
};

class FString
{
public:
	std::string S;
	FString() = default;
	FString(const char* C) : S(C) {}
	FString(const std::string& C) : S(C) {}
	FString(int32 Len, const char* C) : S(C, (size_t)Len) {}
	int32 Len() const { return (int32)S.size(); }
	bool IsEmpty() const { return S.empty(); }
	const char* operator*() const { return S.c_str(); }
	void AppendChar(char C) { S.push_back(C); }
	void Reserve(int32 N) { S.reserve((size_t)N); }
	bool StartsWith(const char* P) const { return S.rfind(P, 0) == 0; }
	bool RemoveFromEnd(const char* P) { std::string p(P); if (S.size() >= p.size() && S.compare(S.size() - p.size(), p.size(), p) == 0) { S.resize(S.size() - p.size()); return true; } return false; }
	FString Mid(int32 Start, int32 Count = INT32_MAX) const { if (Start >= Len()) return FString(); return FString(S.substr((size_t)Start, (size_t)Count)); }
	FString Left(int32 Count) const { return FString(S.substr(0, (size_t)std::max(0, std::min(Count, Len())))); }
	bool operator==(const FString& O) const { return S == O.S; }
	bool operator!=(const FString& O) const { return S != O.S; }
	bool operator<(const FString& O) const { return S < O.S; }
	FString operator+(const FString& O) const { return FString(S + O.S); }
	friend FString operator+(const char* A, const FString& B) { return FString(std::string(A) + B.S); }
	auto begin() const { return S.begin(); }
	auto end() const { return S.end(); }
	template <typename... A> static FString Printf(const char* Fmt, A... Args) { char Buf[4096]; std::snprintf(Buf, sizeof Buf, Fmt, Args...); return FString(Buf); }
	static FString FromInt(int64 V) { return FString(std::to_string(V)); }
};
inline size_t GetTypeHash(const FString& S) { return std::hash<std::string>()(S.S); }

struct FTCHARToUTF8 { std::string S; explicit FTCHARToUTF8(const char* C) : S(C) {} int32 Length() const { return (int32)S.size(); } const char* Get() const { return S.c_str(); } };
struct FUTF8ToTCHAR { std::string S; FUTF8ToTCHAR(const char* C, int32 N) : S(C, (size_t)N) {} int32 Length() const { return (int32)S.size(); } const char* Get() const { return S.c_str(); } };

template <typename T>
class TArray
{
public:
	std::vector<T> V;
	TArray() = default;
	TArray(std::initializer_list<T> L) : V(L) {}
	int32 Num() const { return (int32)V.size(); }
	void Add(const T& X) { V.push_back(X); }
	void Add(T&& X) { V.push_back(std::move(X)); }
	void Reserve(int32 N) { V.reserve((size_t)N); }
	void Reset(int32 N = 0) { V.clear(); V.reserve((size_t)N); }
	void Append(const T* D, int32 N) { V.insert(V.end(), D, D + N); }
	void Append(const TArray& O) { V.insert(V.end(), O.V.begin(), O.V.end()); }
	const T* GetData() const { return V.data(); }
	T* GetData() { return V.data(); }
	T& operator[](int32 I) { return V[(size_t)I]; }
	const T& operator[](int32 I) const { return V[(size_t)I]; }
	auto begin() { return V.begin(); } auto end() { return V.end(); }
	auto begin() const { return V.begin(); } auto end() const { return V.end(); }
	bool operator==(const TArray& O) const { return V == O.V; }
};

template <typename K, typename Val>
class TMap
{
public:
	struct FPair { K Key; Val Value; };
	std::vector<FPair> M;  // insertion-ordered like UE's TMap in practice
	int32 Num() const { return (int32)M.size(); }
	void Reset() { M.clear(); }
	void Empty() { M.clear(); }
	Val* Find(const K& Key) { for (auto& P : M) if (P.Key == Key) return &P.Value; return nullptr; }
	const Val* Find(const K& Key) const { for (auto& P : M) if (P.Key == Key) return &P.Value; return nullptr; }
	Val& Add(const K& Key, Val&& X) { if (Val* V = Find(Key)) { *V = std::move(X); return *V; } M.push_back(FPair{ Key, std::move(X) }); return M.back().Value; }
	Val& Add(const K& Key, const Val& X) { if (Val* V = Find(Key)) { *V = X; return *V; } M.push_back(FPair{ Key, X }); return M.back().Value; }
	Val& FindOrAdd(const K& Key) { if (Val* V = Find(Key)) return *V; M.push_back(FPair{ Key, Val() }); return M.back().Value; }
	bool RemoveAndCopyValue(const K& Key, Val& Out) { for (size_t I = 0; I < M.size(); ++I) if (M[I].Key == Key) { Out = std::move(M[I].Value); M.erase(M.begin() + (long)I); return true; } return false; }
	auto begin() { return M.begin(); } auto end() { return M.end(); }
	auto begin() const { return M.begin(); } auto end() const { return M.end(); }
	auto CreateConstIterator() const { return M.begin(); }
	bool operator==(const TMap& O) const { if (M.size() != O.M.size()) return false; for (auto& P : M) { const Val* V = O.Find(P.Key); if (!V || !(*V == P.Value)) return false; } return true; }
};

template <typename T>
class TOptional
{
public:
	std::optional<T> O;
	TOptional() = default;
	TOptional(const T& V) : O(V) {}
	TOptional(T&& V) : O(std::move(V)) {}
	bool IsSet() const { return O.has_value(); }
	const T& GetValue() const { return *O; }
	T& GetValue() { return *O; }
	void Reset() { O.reset(); }
	TOptional& operator=(const T& V) { O = V; return *this; }
	TOptional& operator=(T&& V) { O = std::move(V); return *this; }
	bool operator==(const TOptional& R) const { return O == R.O; }
};

template <typename T>
class TSharedPtr
{
public:
	std::shared_ptr<T> P;
	TSharedPtr() = default;
	TSharedPtr(std::nullptr_t) {}
	TSharedPtr(const std::shared_ptr<T>& In) : P(In) {}
	template <typename U> TSharedPtr(const TSharedPtr<U>& In) : P(In.P) {}
	bool IsValid() const { return (bool)P; }
	void Reset() { P.reset(); }
	T* Get() const { return P.get(); }
	T* operator->() const { return P.get(); }
	T& operator*() const { return *P; }
	const TSharedPtr& ToSharedRef() const { return *this; }
	explicit operator bool() const { return (bool)P; }
	bool operator==(const TSharedPtr& O) const { return P == O.P; }
};
template <typename T> using TSharedRef = TSharedPtr<T>;
template <typename T> using TWeakPtr = std::weak_ptr<T>;
template <typename T, typename... A> TSharedPtr<T> MakeShared(A&&... Args) { return TSharedPtr<T>(std::make_shared<T>(std::forward<A>(Args)...)); }
template <typename T> struct TSharedFromThis : std::enable_shared_from_this<T> {};
