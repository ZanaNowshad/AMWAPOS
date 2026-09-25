Bundled offline OCR models (Tesseract LSTM, integerized "best"):

| File | Language | SHA-256 |
| --- | --- | --- |
| eng.traineddata.gz | English | 45b4cb346724ac1774f1c36f42f182b887bcdb28ebe63e6fff90ac41f3fcff91 |
| ara.traineddata.gz | Arabic | f4746c44b02342dd5b3d4f0198000f47d7c49f1a229e63e0f436c0592dcd9639 |

Source: npm `@tesseract.js-data/eng@1.0.0` and `@tesseract.js-data/ara@1.0.0` (`4.0.0_best_int`),
which repackage tesseract-ocr/tessdata_best. License: Apache-2.0.

The OCR worker (`crates/amwapos-hub/src/ocr_worker.rs`) checks each file against `models.json`,
unpacks it into `<data>/ocr/tessdata` and runs the bundled Tesseract with it. OCR reports
`ocr_model_missing` and stays disabled when the English model is missing or its hash does not match.
