package com.example.controller;

import com.example.dto.Result;
import jakarta.servlet.http.Part;
import java.util.List;
import org.springframework.http.MediaType;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RequestPart;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.web.multipart.MultipartFile;

/**
 * 附件上传。
 */
@RestController
@RequestMapping("/api/upload")
public class UploadController {

    /**
     * 上传一个文件及其元数据。
     */
    @PostMapping(value = "/file", consumes = MediaType.MULTIPART_FORM_DATA_VALUE)
    public Result<String> upload(@RequestPart("file") MultipartFile file,
                                 @RequestPart(value = "metadata", required = false) String metadata) {
        return Result.ok(file.getOriginalFilename() + metadata);
    }

    /**
     * 批量上传：集合形态的文件字段，`List<MultipartFile>` 与 servlet `Part` 两种写法。
     */
    @PostMapping(value = "/files", consumes = MediaType.MULTIPART_FORM_DATA_VALUE)
    public Result<Integer> uploadMany(@RequestPart("files") List<MultipartFile> files,
                                      @RequestPart("parts") List<Part> parts,
                                      @RequestPart(value = "note", required = false) String note) {
        return Result.ok(files.size() + parts.size() + (note == null ? 0 : 1));
    }
}
