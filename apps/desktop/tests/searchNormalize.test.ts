import test from "node:test";
import assert from "node:assert/strict";
import {
  extractKoreanChoseong,
  isImeComposingEvent,
  isKoreanChoseongQuery,
  looksLikeInternationalPhoneQuery,
  matchesSearchQuery,
  normalizeForSearch,
} from "../src/utils/searchNormalize.ts";

test("normalizeForSearch strips Vietnamese/Latin diacritics without decomposing Hangul or Kana", () => {
  assert.equal(normalizeForSearch("Nguyễn Văn Minh"), "nguyen van minh");
  assert.equal(normalizeForSearch("Đà Nẵng — ĐỨC DŨNG"), "da nang — duc dung");

  // NFD Vietnamese normalizes identically to NFC Vietnamese
  const nfdVi = "Nguy\u0065\u0302\u0303n Va\u0306n \u0110u\u031B\u0301c";
  assert.equal(normalizeForSearch(nfdVi), "nguyen van duc");

  // NFC Hangul stays intact as NFC syllables (never decomposed into Jamo)
  assert.equal(normalizeForSearch("김민수 부장님"), "김민수 부장님");
  assert.equal(normalizeForSearch("안").length, 1);
  assert.notEqual(normalizeForSearch("안"), normalizeForSearch("아"));
  assert.equal(normalizeForSearch("안").includes(normalizeForSearch("아")), false);

  // NFD Hangul Jamo composes cleanly to canonical NFC Hangul
  const nfdHangul = "\u1112\u1161\u11AB\u1100\u1173\u11AF"; // 한글
  assert.equal(normalizeForSearch(nfdHangul), "한글");
  assert.equal(normalizeForSearch(nfdHangul).length, 2);

  // Japanese Kana dakuten/handakuten stay intact
  assert.equal(normalizeForSearch("がぎぐ パピプ"), "がぎぐ パピプ");

  // Mixed Korean + Vietnamese + English
  assert.equal(normalizeForSearch("Team 회의 Báo Cáo"), "team 회의 bao cao");
  assert.equal(normalizeForSearch(""), "");
});

test("extractKoreanChoseong and isKoreanChoseongQuery handle all 19 consonants, NFD, and non-choseong inputs", () => {
  assert.equal(extractKoreanChoseong("김민수"), "ㄱㅁㅅ");
  assert.equal(extractKoreanChoseong("회의실"), "ㅎㅇㅅ");
  assert.equal(extractKoreanChoseong("회의실에서 만나요"), "ㅎㅇㅅㅇㅅ ㅁㄴㅇ");
  assert.equal(extractKoreanChoseong("김민수 부장님"), "ㄱㅁㅅ ㅂㅈㄴ");

  // All 19 modern initial consonants in Unicode order
  assert.equal(
    extractKoreanChoseong("가까나다따라마바빠사싸아자짜차카타파하"),
    "ㄱㄲㄴㄷㄸㄹㅁㅂㅃㅅㅆㅇㅈㅉㅊㅋㅌㅍㅎ",
  );

  // NFD Hangul composes before extraction
  const nfdHangul = "\u1112\u1161\u11AB\u1100\u1173\u11AF"; // 한글
  assert.equal(extractKoreanChoseong(nfdHangul), "ㅎㄱ");

  // Non-Hangul chars are preserved in place
  assert.equal(extractKoreanChoseong("김민수 (PM) 2026"), "ㄱㅁㅅ (PM) 2026");
  assert.equal(extractKoreanChoseong(""), "");

  // Choseong query detection
  assert.equal(isKoreanChoseongQuery("ㄱㅁㅅ"), true);
  assert.equal(isKoreanChoseongQuery("ㅎㅇ"), true);
  assert.equal(isKoreanChoseongQuery("ㄱㅁㅅ ㅂㅈ"), true);
  assert.equal(isKoreanChoseongQuery("ㄱㄲㄴㄷㄸㄹㅁㅂㅃㅅㅆㅇㅈㅉㅊㅋㅌㅍㅎ"), true);

  assert.equal(isKoreanChoseongQuery(""), false);
  assert.equal(isKoreanChoseongQuery("   \t\n"), false);
  assert.equal(isKoreanChoseongQuery("김민수"), false);
  assert.equal(isKoreanChoseongQuery("ㄱㅁ1"), false);
  assert.equal(isKoreanChoseongQuery("ㅏㅓㅗㅜ"), false); // vowels
  assert.equal(isKoreanChoseongQuery("ㄳ"), false); // compound jongseong
  assert.equal(isKoreanChoseongQuery("abc"), false);
});

test("matchesSearchQuery supports Vietnamese, Korean substrings, Choseong, NFD/NFC, and multi-token queries", () => {
  // Required core cases
  assert.equal(matchesSearchQuery("Nguyễn Văn Minh", "nguyen minh"), true);
  assert.equal(matchesSearchQuery("김민수 부장님", "부장"), true);
  assert.equal(matchesSearchQuery("김민수", "ㄱㅁㅅ"), true);
  assert.equal(matchesSearchQuery("회의실에서", "회의"), true);
  assert.equal(matchesSearchQuery("회의실에서", "실"), true);

  // NFD Hangul matches NFC Hangul in both directions
  const nfdHangul = "\u1112\u1161\u11AB\u1100\u1173\u11AF"; // 한글
  assert.equal(matchesSearchQuery("한글", nfdHangul), true);
  assert.equal(matchesSearchQuery(nfdHangul, "한글"), true);
  assert.equal(matchesSearchQuery(nfdHangul, "ㅎㄱ"), true);

  // No NFD Hangul false positive ("아" must NOT match "안")
  assert.equal(matchesSearchQuery("안", "아"), false);
  assert.equal(matchesSearchQuery("안녕하세요", "아녕"), false);

  // Mixed Choseong + Hangul/Latin multi-token AND
  assert.equal(matchesSearchQuery("김민수 부장님 (Dev)", "ㄱㅁㅅ 부장"), true);
  assert.equal(matchesSearchQuery("김민수 부장님 (Dev)", "ㄱㅁㅅ dev"), true);
  assert.equal(matchesSearchQuery("김민수 부장님", "ㄱㅁㅅ 과장"), false);

  // Multi-token Choseong
  assert.equal(matchesSearchQuery("김민수 부장님", "ㄱㅁㅅ ㅂㅈ"), true);
  assert.equal(matchesSearchQuery("김민수 부장님", "ㄱㅁㅅ ㅎㅇ"), false);

  // Empty / whitespace query matches all; non-empty query does not match empty haystack
  assert.equal(matchesSearchQuery("김민수", ""), true);
  assert.equal(matchesSearchQuery("김민수", "   "), true);
  assert.equal(matchesSearchQuery("", "부장"), false);
});

test("isImeComposingEvent detects React nativeEvent composition, DOM composition, and keyCode 229", () => {
  assert.equal(isImeComposingEvent({ nativeEvent: { isComposing: true, keyCode: 13 } }), true);
  assert.equal(isImeComposingEvent({ nativeEvent: { isComposing: false, keyCode: 229 } }), true);
  assert.equal(isImeComposingEvent({ keyCode: 229 }), true);
  assert.equal(isImeComposingEvent({ isComposing: true, keyCode: 13 }), true);

  assert.equal(isImeComposingEvent({ keyCode: 13, nativeEvent: { isComposing: false, keyCode: 13 } }), false);
  assert.equal(isImeComposingEvent({}), false);
  assert.equal(isImeComposingEvent(null), false);
  assert.equal(isImeComposingEvent(undefined), false);
});

test("looksLikeInternationalPhoneQuery accepts VN, Korea, and E.164 numbers and rejects non-phone queries", () => {
  // Vietnam
  assert.equal(looksLikeInternationalPhoneQuery("0912345678"), true);
  assert.equal(looksLikeInternationalPhoneQuery("0912 345 678"), true);
  assert.equal(looksLikeInternationalPhoneQuery("+84 912 345 678"), true);
  assert.equal(looksLikeInternationalPhoneQuery("84912345678"), true);
  assert.equal(looksLikeInternationalPhoneQuery("912345678"), true);

  // Korea
  assert.equal(looksLikeInternationalPhoneQuery("+82 10-1234-5678"), true);
  assert.equal(looksLikeInternationalPhoneQuery("+82-10-1234-5678"), true);
  assert.equal(looksLikeInternationalPhoneQuery("+82 010-1234-5678"), true);
  assert.equal(looksLikeInternationalPhoneQuery("821012345678"), true);
  assert.equal(looksLikeInternationalPhoneQuery("010-1234-5678"), true);
  assert.equal(looksLikeInternationalPhoneQuery("010 1234 5678"), true);
  assert.equal(looksLikeInternationalPhoneQuery("010.1234.5678"), true);
  assert.equal(looksLikeInternationalPhoneQuery("02-1234-5678"), true);
  assert.equal(looksLikeInternationalPhoneQuery("(010) 1234-5678"), true);

  // General E.164 (+<7..15 digits>)
  assert.equal(looksLikeInternationalPhoneQuery("+1 (415) 555-2671"), true);
  assert.equal(looksLikeInternationalPhoneQuery("+81 90-1234-5678"), true);
  assert.equal(looksLikeInternationalPhoneQuery("+44 20 7946 0958"), true);
  assert.equal(looksLikeInternationalPhoneQuery("+1234567"), true); // 7 digits
  assert.equal(looksLikeInternationalPhoneQuery("+123456789012345"), true); // 15 digits

  // Reject non-phone strings
  assert.equal(looksLikeInternationalPhoneQuery(""), false);
  assert.equal(looksLikeInternationalPhoneQuery("   "), false);
  assert.equal(looksLikeInternationalPhoneQuery("2026"), false);
  assert.equal(looksLikeInternationalPhoneQuery("+123456"), false); // 6 digits < 7
  assert.equal(looksLikeInternationalPhoneQuery("+1234567890123456"), false); // 16 digits > 15
  assert.equal(looksLikeInternationalPhoneQuery("+0123456789"), false); // country code cannot start with 0
  assert.equal(looksLikeInternationalPhoneQuery("1700000000000"), false); // 13-digit ms timestamp
  assert.equal(looksLikeInternationalPhoneQuery("김민수"), false);
  assert.equal(looksLikeInternationalPhoneQuery("010-abcd-5678"), false);
  assert.equal(looksLikeInternationalPhoneQuery("123+4567890"), false);
});
