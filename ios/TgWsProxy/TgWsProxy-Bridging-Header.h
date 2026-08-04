#ifndef TgWsProxy_Bridging_Header_h
#define TgWsProxy_Bridging_Header_h

/// C ABI exported by `crates/ios-bridge`.
///
/// Every function returns a heap allocated UTF-8 string that the caller must
/// release with `tgws_free_string`.

char *tgws_start(const char *config_json);
char *tgws_stop(void);
char *tgws_status(void);
void tgws_free_string(char *pointer);

#endif /* TgWsProxy_Bridging_Header_h */
