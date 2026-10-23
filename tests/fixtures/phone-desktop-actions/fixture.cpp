// The SDK validates the keyboard ABI. These exports never call SendInput or
// inspect physical keys; only supplied fixture values enter their trace.
#include <windows.h>
#include <cstddef>
#include <string>
static_assert(sizeof(INPUT)==40 && offsetof(INPUT,ki)==8);
static_assert(offsetof(KEYBDINPUT,wVk)==0 && offsetof(KEYBDINPUT,wScan)==2);
static_assert(offsetof(KEYBDINPUT,dwFlags)==4 && offsetof(KEYBDINPUT,time)==8 && offsetof(KEYBDINPUT,dwExtraInfo)==16);
static std::string trace;
static int pressed=-1,word=0,sends=0;
static UINT first=0,second=0;
extern "C" __declspec(dllexport) void FixtureReset(int key,int state,UINT sent,UINT released){trace.clear();pressed=key;word=state;sends=0;first=sent;second=released;}
extern "C" __declspec(dllexport) SHORT WINAPI FixtureKeyState(int key){trace+="key:"+std::to_string(key)+";";return static_cast<SHORT>(key==pressed?word:0);}
extern "C" __declspec(dllexport) UINT WINAPI FixtureSendInput(UINT count,const INPUT* inputs,int size){
    trace+="send:"+std::to_string(count)+":"+std::to_string(size)+";";
    if(size!=sizeof(INPUT)||count>128)return 0;
    for(UINT i=0;i<count;i++){const auto& key=inputs[i].ki;trace+="input:"+std::to_string(inputs[i].type)+":"+std::to_string(key.wVk)+":"+std::to_string(key.wScan)+":"+std::to_string(key.dwFlags)+":"+std::to_string(key.time)+":"+std::to_string(key.dwExtraInfo)+";";}
    return sends++==0?first:second;
}
extern "C" __declspec(dllexport) const char* FixtureTrace(){return trace.c_str();}
