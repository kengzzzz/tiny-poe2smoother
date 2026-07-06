// extern "C" wrappers so Rust binds only unmangled symbols.
// Ooz_Decompress is already extern "C" in kraken.cpp and needs no wrapper.
#include "stdafx.h"
#include "compress.h"

// Defined in compress.cpp but not declared in any header.
int GetCompressedBufferSizeNeeded(int size);

extern "C" {

int Ooz_CompressBlock(int codec_id, uint8 *src_in, uint8 *dst_in, int src_size,
                      int level, const CompressOptions *compressopts,
                      uint8 *src_window_base, LRMCascade *lrm) {
  return CompressBlock(codec_id, src_in, dst_in, src_size, level, compressopts,
                       src_window_base, lrm);
}

int Ooz_GetCompressedBufferSizeNeeded(int size) {
  return GetCompressedBufferSizeNeeded(size);
}

} // extern "C"
