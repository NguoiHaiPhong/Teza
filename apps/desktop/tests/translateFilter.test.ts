import test from "node:test";
import assert from "node:assert/strict";
import {
  classifySkipTranslation,
  isEffectivelySameText,
  isSameLangCode,
  normalizeLangBase,
} from "../src/utils/translateFilter.ts";

test("classifySkipTranslation detects already-Vietnamese messages targeting VI", () => {
  assert.equal(classifySkipTranslation("Hôm nay họp lúc 2 giờ chiều nhé", "VI"), "same_language");
  assert.equal(classifySkipTranslation("Cảm ơn bạn nhiều lắm :))", "VI"), "same_language");
  assert.equal(classifySkipTranslation("@Minh ơi chiều nay rảnh không?", "VI"), "same_language");
  assert.equal(classifySkipTranslation("khong duoc dau nha hom nay ban roi", "VI"), "same_language");
  assert.equal(classifySkipTranslation("minh gui file roi nha", "VI"), "same_language");
  // Short ambiguous unaccented Vietnamese without unambiguous markers goes through API
  assert.equal(classifySkipTranslation("ban cho toi xem", "VI"), null);
  // NFD input is normalized cleanly
  assert.equal(
    classifySkipTranslation("Hôm nay họp vui quá".normalize("NFD"), "VI"),
    "same_language",
  );
});

test("classifySkipTranslation distinguishes mixed VI+tech terms vs mixed VI+English clauses", () => {
  assert.equal(classifySkipTranslation("Mình deploy server mới rồi nhé", "VI"), "same_language");
  assert.equal(
    classifySkipTranslation(
      "Bạn xem giúp mình: the server returned an error and please check your account",
      "VI",
    ),
    null,
  );
  assert.equal(
    classifySkipTranslation("The meeting is scheduled for tomorrow morning", "VI"),
    null,
  );
  // French / other Latin languages must not be misclassified as Vietnamese
  assert.equal(classifySkipTranslation("Très bien à bientôt mon ami", "VI"), null);
});

test("classifySkipTranslation detects EN, KO, JA, ZH, RU, TH and variant codes", () => {
  assert.equal(
    classifySkipTranslation("The meeting is scheduled for tomorrow morning", "EN"),
    "same_language",
  );
  assert.equal(classifySkipTranslation("Thank you", "EN-US"), "same_language");
  assert.equal(classifySkipTranslation("Hello", "EN-GB"), "same_language");
  assert.equal(classifySkipTranslation("Hôm nay trời đẹp quá", "EN"), null);

  assert.equal(classifySkipTranslation("안녕하세요 오늘 회의 있어요?", "KO"), "same_language");
  assert.equal(classifySkipTranslation("안녕하세요 오늘 회의 있어요?", "VI"), null);

  assert.equal(classifySkipTranslation("明日の会議は10時です", "JA"), "same_language");
  assert.equal(classifySkipTranslation("明天的会议十点开始", "ZH"), "same_language");
  assert.equal(classifySkipTranslation("明天的会议十点开始", "ZH-HANT"), null);

  assert.equal(classifySkipTranslation("Привет, как дела сегодня?", "RU"), "same_language");
  assert.equal(classifySkipTranslation("สวัสดีครับ วันนี้ว่างไหม", "TH"), "same_language");
});

test("classifySkipTranslation marks placeholders, URLs, mentions, and emojis as non_linguistic", () => {
  assert.equal(classifySkipTranslation("https://teza.app/docs?v=1", "VI"), "non_linguistic");
  assert.equal(classifySkipTranslation("test@example.com", "EN"), "non_linguistic");
  assert.equal(classifySkipTranslation("@Minh 12345", "VI"), "non_linguistic");
  assert.equal(classifySkipTranslation("👍👍 =)) :D XD", "VI"), "non_linguistic");
  assert.equal(classifySkipTranslation("[Hình ảnh]", "VI"), "non_linguistic");
  assert.equal(classifySkipTranslation("BUZZ!", "EN"), "non_linguistic");
  assert.equal(
    classifySkipTranslation("[Tập tin] Todolist 2026.11.20.xlsx", "VI"),
    "non_linguistic",
  );
  assert.equal(
    classifySkipTranslation("[Tập tin] Todolist 2026.11.20.xlsx", "EN"),
    "non_linguistic",
  );
  assert.equal(
    classifySkipTranslation("[Tập tin] Todolist.xlsx\nGửi giúp mình nhé", "VI"),
    "same_language",
  );
  assert.equal(classifySkipTranslation("ICON25", "VI"), "non_linguistic");
  assert.equal(classifySkipTranslation("ICON25 SF", "EN"), "non_linguistic");
  assert.equal(classifySkipTranslation("EBR28193301\t6L1T03X6-0003\t351", "VI"), "non_linguistic");
  assert.equal(classifySkipTranslation("EAE65236402\tFOC1-BOX-10", "VI"), "non_linguistic");

  // ALL-CAPS natural English sentences containing alphanumeric times/codes must still translate
  assert.equal(classifySkipTranslation("MEETING AT 10AM TOMORROW", "VI"), null);
  assert.equal(classifySkipTranslation("PLEASE CHECK ICON25 STATUS BEFORE 5PM", "VI"), null);
  assert.equal(classifySkipTranslation("CALL ME AT 5PM", "VI"), null);
});

test("classifySkipTranslation strips multi-word mentions and handles contractions & technical acronyms", () => {
  assert.equal(classifySkipTranslation("@Đào Nụ please share moving plan", "VI"), null);
  assert.equal(classifySkipTranslation("@Đào Nụ please share moving plan", "EN"), "same_language");
  assert.equal(classifySkipTranslation("Dear Ms. Đào Nụ, please verify this", "VI"), null);
  assert.equal(
    classifySkipTranslation("Dear Ms. Đào Nụ, please verify this", "EN"),
    "same_language",
  );
  assert.equal(
    classifySkipTranslation("@Ahn Bm I'm still making, I'll send after finish , sir", "EN"),
    "same_language",
  );
  assert.equal(
    classifySkipTranslation("@Ahn Bm I'm still making, I'll send after finish , sir", "VI"),
    null,
  );
  assert.equal(
    classifySkipTranslation("@Nguyễn Thanh R&D San together up-date pls", "EN"),
    "same_language",
  );
  assert.equal(
    classifySkipTranslation("@Vu Thi Anh RD 인원 확인하고 있나요?", "KO"),
    "same_language",
  );
  assert.equal(
    classifySkipTranslation("@Khangdx @Vu Thi Anh iconic NG는 무슨 불량인지?", "KO"),
    "same_language",
  );
  assert.equal(
    classifySkipTranslation(
      "@Vu Thi Anh ??. PICKUP LOSS 중 7~10월 10,000$ 증가한 사유 분석해서 보고 할 것.",
      "KO",
    ),
    "same_language",
  );
  assert.equal(
    classifySkipTranslation(
      "@HCường , @Doan Hoan , @Thang ND mn vào họp với mr Kang nhé.",
      "VI",
    ),
    "same_language",
  );
  assert.equal(
    classifySkipTranslation("@Vu Thi Anh underfill a gửi tuần trước rồi e cho done giúp a nhé", "VI"),
    "same_language",
  );
});

test("normalizeLangBase, isSameLangCode, and isEffectivelySameText handle edge cases", () => {
  assert.equal(normalizeLangBase("en-US"), "EN");
  assert.equal(normalizeLangBase("zh-Hans"), "ZH");
  assert.equal(normalizeLangBase("zh-TW"), "ZH-HANT");

  assert.equal(isSameLangCode("en-US", "EN"), true);
  assert.equal(isSameLangCode("VI", "vi"), true);
  assert.equal(isSameLangCode("zh-TW", "ZH"), true);
  assert.equal(isSameLangCode("EN", "VI"), false);
  assert.equal(isSameLangCode(null, "VI"), false);

  assert.equal(isEffectivelySameText("Xin   chào ", "xin chào"), true);
  assert.equal(isEffectivelySameText("Hello", ""), true);
  assert.equal(isEffectivelySameText("Hello", "Xin chào"), false);
});
